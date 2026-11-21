use super::*;
use alpheus_commun::temp_path::TempPath;

#[test]
fn the_first_reading_seeds() {
    let mut s = Shield::default();
    assert_eq!(s.observe(Some(true)), None);
    assert_eq!(s.state(), Some(true));
    let mut s = Shield::default();
    assert_eq!(s.observe(None), None, "unknown is no seed");
    assert_eq!(s.observe(Some(false)), None, "the first known reading seeds");
    assert_eq!(s.observe(Some(true)), Some(Edge::On));
}

#[test]
fn a_change_is_an_edge() {
    let mut s = Shield::default();
    s.observe(Some(false));
    assert_eq!(s.observe(Some(false)), None, "same state");
    assert_eq!(s.observe(Some(true)), Some(Edge::On));
    assert_eq!(s.observe(Some(true)), None);
    assert_eq!(s.observe(Some(false)), Some(Edge::Off));
}

#[test]
fn unknown_in_between_changes_nothing() {
    let mut s = Shield::default();
    s.observe(Some(true));
    assert_eq!(s.observe(None), None);
    assert_eq!(s.state(), None);
    assert_eq!(s.observe(Some(true)), None, "on, unknown, on");
    s.observe(None);
    assert_eq!(s.observe(Some(false)), Some(Edge::Off), "on, unknown, off");
}

#[test]
fn the_unknown_spell_is_noted_once() {
    let mut s = Shield::default();
    s.observe(Some(true));
    assert!(!s.first_unknown(), "known: nothing to note");
    s.observe(None);
    assert!(s.first_unknown());
    assert!(!s.first_unknown(), "once per spell");
    s.observe(Some(true));
    s.observe(None);
    assert!(s.first_unknown(), "a new spell");
}

#[test]
fn read_flag_tells_missing_from_unreadable() {
    let d = TempPath::dir("shield-flag");
    let flag = d.as_ref().join("on");
    assert_eq!(read_flag(&flag), Some(false));
    std::fs::write(&flag, "").unwrap();
    assert_eq!(read_flag(&flag), Some(true));
    // A parent that is a plain file: the answer is an error, not "off".
    let plain = d.as_ref().join("plain");
    std::fs::write(&plain, "").unwrap();
    assert_eq!(read_flag(&plain.join("on")), None);
}

#[test]
fn the_view_follows_the_config() {
    let mut c = ShieldConfig::default();
    let s = Shield::default();
    let v = s.view(&c, Instant::now());
    assert_eq!((v.state, v.can_list, v.can_allow), (None, true, false), "USBGuard's list by default");
    c.blocked = Some(vec![]);
    c.allow = vec!["x".into(), "{id}".into()];
    let v = s.view(&c, Instant::now());
    assert_eq!((v.can_list, v.can_allow), (false, true));
}
