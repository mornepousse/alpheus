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

#[test]
fn a_shield_notice_is_alphas_own_and_normal() {
    use crate::notify::Urgency;
    let c = ShieldConfig { on_text: Some("Up".into()), ..Default::default() };
    let n = incoming(&NoticeKind::On, &c);
    assert_eq!((n.app_name.as_str(), n.summary.as_str(), n.body.as_str()), ("alpheus", "Shield on", "Up"));
    assert_eq!((n.urgency, n.expire_ms, n.replaces_id), (Urgency::Normal, -1, 0));
    assert!(n.actions.is_empty() && !n.resident && !n.transient);
    let n = incoming(&NoticeKind::Allowed("Test Stick".into()), &c);
    assert_eq!((n.summary.as_str(), n.body.as_str()), ("USB device allowed", "Test Stick"));
}
