use super::*;
use crate::watch::Watches;
use alpheus_commun::temp_path::TempPath;

#[test]
fn unconfigured_adds_no_watch() {
    // No ShieldApp without a configured [shield]...
    assert!(ShieldApp::new(&ShieldConfig::default()).is_none());
    let partial = ShieldConfig { flag: "/var/lib/test-shield/on".into(), ..Default::default() };
    assert!(ShieldApp::new(&partial).is_none());
    // ...so watch::start passes no flag, and the instance carries no shield target.
    let theme = TempPath::dir("shield-app-theme");
    let w = Watches::start(theme.as_ref(), None).unwrap();
    assert!(!w.shield_armed());
    let c = ShieldConfig { flag: "/var/lib/test-shield/on".into(), on: vec!["x".into()], off: vec!["x".into()], ..Default::default() };
    assert!(ShieldApp::new(&c).is_some());
}
