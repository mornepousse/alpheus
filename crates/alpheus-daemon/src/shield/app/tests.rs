//! Tests of Shield mode's wiring (`app.rs`): nothing exists unconfigured, and a notice is alpheus's own.

use super::*;
use crate::watch::Watches;
use alpheus_common::temp_path::TempPath;

#[test]
fn an_unconfigured_shield_adds_no_watch() {
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
fn a_shield_notice_comes_from_alpheus_at_normal_urgency() {
    use crate::notify::Urgency;
    let c = ShieldConfig { on_text: Some("Up".into()), ..Default::default() };
    let n = incoming(&NoticeKind::On, &c);
    assert_eq!((n.app_name.as_str(), n.summary.as_str(), n.body.as_str()), ("alpheus", "Shield on", "Up"));
    assert_eq!((n.urgency, n.expire_ms, n.replaces_id), (Urgency::Normal, -1, 0));
    assert!(n.actions.is_empty() && !n.resident && !n.transient);
    let n = incoming(&NoticeKind::Allowed("Test Stick".into()), &c);
    assert_eq!((n.summary.as_str(), n.body.as_str()), ("USB device allowed", "Test Stick"));
}

#[test]
fn the_flag_counts_as_watched_only_when_the_theme_watch_armed_its_target() {
    assert!(!flag_is_watched(None), "no theme watch at all");
    let theme = TempPath::dir("shield-app-watched-theme");
    let without = Watches::start(theme.as_ref(), None).unwrap();
    assert!(!flag_is_watched(Some(&without)), "a watch with no flag target");
    let flags = TempPath::dir("shield-app-watched-flag");
    let flag = flags.as_ref().join("on");
    let with = Watches::start(theme.as_ref(), Some(flag.as_path())).unwrap();
    assert!(flag_is_watched(Some(&with)), "a watch whose flag target armed");
}

#[test]
fn a_click_on_the_row_switches_to_the_other_state_and_does_nothing_when_unknown() {
    let view = |state| Some(ShieldView { state, switching: false, failed_until: None, can_list: true, can_allow: true });
    assert_eq!(toggle_target(view(Some(true))), Some(false));
    assert_eq!(toggle_target(view(Some(false))), Some(true));
    assert_eq!(toggle_target(view(None)), None, "unknown");
    assert_eq!(toggle_target(None), None, "no view yet");
}

#[test]
fn a_worker_starts_only_when_the_loops_mail_channel_exists() {
    use alpheus_common::exec::fake::FakeExecutor;
    let exec: std::sync::Arc<dyn alpheus_common::exec::Executor> = std::sync::Arc::new(FakeExecutor::new());
    assert!(!start_worker(None, exec.clone(), Job::List, 0, vec!["usbguard".into()]), "no channel, no worker");
    let (tx, _rx) = smithay_client_toolkit::reexports::calloop::channel::channel::<Mail>();
    assert!(start_worker(Some(tx), exec, Job::List, 0, vec!["usbguard".into()]));
}
