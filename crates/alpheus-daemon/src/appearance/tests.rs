use super::*;
use crate::session_bus::bus::testing::FakeOutbox;

#[test]
fn the_rule_names_one_signal_of_the_portal_for_one_key() {
    assert_eq!(
        SETTING_RULE,
        format!(
            "type='signal',sender='{PORTAL_NAME}',path='{PORTAL_PATH}',interface='{SETTINGS_INTERFACE}',member='SettingChanged',arg0='{APPEARANCE_NAMESPACE}',arg1='{COLOR_SCHEME_KEY}'"
        )
    );
}

#[test]
fn start_sends_the_rule_then_one_read_one() {
    let mut out = FakeOutbox::default();
    let a = Appearance::start(&mut out);
    assert_eq!(out.sent, [Call::AddMatch { rule: SETTING_RULE.into() }], "the rule: no reply expected, nothing pending");
    assert_eq!(out.calls, [(CallId(1), Call::ReadOne)], "one call, pending with its deadline");
    assert!(a.owns(CallId(1)));
    assert_eq!(a.portal(), None, "nothing known before the answer: effective_mode reads that as dark");
}

#[test]
fn the_answer_gives_the_mode_once() {
    let mut out = FakeOutbox::default();
    let mut a = Appearance::start(&mut out);
    assert_eq!(a.answer(CallId(1), Ok(Reply::ColorScheme(1))), Some(Mode::Dark));
    assert_eq!(a.portal(), Some(Mode::Dark));
    assert!(!a.owns(CallId(1)), "answered: nothing left to wait for");
    assert_eq!(a.answer(CallId(1), Ok(Reply::ColorScheme(2))), None, "the same id again is not ours any more");
    assert_eq!(a.portal(), Some(Mode::Dark));
}

#[test]
fn signals_change_the_mode_only_when_it_changes() {
    let mut a = Appearance::default();
    assert_eq!(a.changed(2), Some(Mode::Light));
    assert_eq!(a.changed(2), None, "the same value again: nothing to repaint");
    assert_eq!(a.changed(0), Some(Mode::Dark), "0, no preference, is dark");
    assert_eq!(a.changed(7), None, "an unknown value is dark too: unchanged");
}

#[test]
fn a_failed_read_leaves_no_mode_and_nothing_pending() {
    for e in [
        CallError::Timeout,
        CallError::Disconnected,
        CallError::Remote("org.freedesktop.DBus.Error.ServiceUnknown".into()),
        CallError::BadReply("color-scheme is Some(\"s\")".into()),
    ] {
        let mut out = FakeOutbox::default();
        let mut a = Appearance::start(&mut out);
        assert_eq!(a.answer(CallId(1), Err(e.clone())), None, "{e:?}");
        assert_eq!(a.portal(), None, "{e:?}");
        assert!(!a.owns(CallId(1)), "{e:?}: nothing pending");
        assert_eq!(effective_mode(ModeSetting::Auto, a.portal()), Mode::Dark, "{e:?}");
    }
}

#[test]
fn another_call_is_not_ours() {
    let mut out = FakeOutbox::default();
    out.call(Call::ListNames);
    let mut a = Appearance::start(&mut out);
    assert!(!a.owns(CallId(1)));
    assert_eq!(a.answer(CallId(1), Ok(Reply::ColorScheme(2))), None);
    assert_eq!(a.portal(), None);
}

#[test]
fn the_portal_is_wanted_only_in_auto_and_once() {
    assert!(!portal_wanted(ModeSetting::Dark, false));
    assert!(!portal_wanted(ModeSetting::Light, false));
    assert!(!portal_wanted(ModeSetting::Auto, true), "tried once: never again");
    assert!(portal_wanted(ModeSetting::Auto, false));
}

#[test]
fn a_repaint_only_when_the_effective_mode_changes() {
    assert!(!repaint_needed(ModeSetting::Auto, None, Mode::Dark), "no portal was already dark");
    assert!(repaint_needed(ModeSetting::Auto, None, Mode::Light));
    assert!(repaint_needed(ModeSetting::Auto, Some(Mode::Light), Mode::Dark));
    assert!(!repaint_needed(ModeSetting::Dark, None, Mode::Light), "a fixed mode ignores the portal");
    assert!(!repaint_needed(ModeSetting::Light, Some(Mode::Dark), Mode::Light));
}

#[test]
fn the_mode_is_published_once_then_on_each_change() {
    assert_eq!(mode_to_publish(None, Mode::Dark), Some(Mode::Dark), "startup: dark is written too");
    assert_eq!(mode_to_publish(Some(Mode::Dark), Mode::Dark), None, "unchanged: nothing written");
    assert_eq!(mode_to_publish(Some(Mode::Dark), Mode::Light), Some(Mode::Light));
}

#[test]
fn the_mode_file_is_written_whole_in_an_owner_only_directory() {
    use alpheus_common::theme::mode_file::read_mode;
    use std::os::unix::fs::PermissionsExt;
    let run = std::env::temp_dir().join(format!("alpheus-{}-mode-write", std::process::id()));
    let _ = std::fs::remove_dir_all(&run);
    std::fs::create_dir_all(&run).unwrap();
    write_mode(&run, Mode::Light).unwrap();
    let dir = run.join("alpheus");
    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "owner-only");
    assert_eq!(std::fs::read_to_string(dir.join("mode")).unwrap(), "light\n");
    write_mode(&run, Mode::Dark).unwrap();
    assert_eq!(read_mode(Some(&run)), Mode::Dark, "replaced");
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["mode"], "no temporary file left behind");
    let _ = std::fs::remove_dir_all(&run);
}

#[test]
fn a_directory_open_to_others_or_a_symlink_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let run = std::env::temp_dir().join(format!("alpheus-{}-mode-unsafe", std::process::id()));
    let _ = std::fs::remove_dir_all(&run);
    std::fs::create_dir_all(run.join("alpheus")).unwrap();
    std::fs::set_permissions(run.join("alpheus"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(write_mode(&run, Mode::Light).is_err(), "0755 refused");
    assert!(!run.join("alpheus/mode").exists());
    std::fs::set_permissions(run.join("alpheus"), std::fs::Permissions::from_mode(0o700)).unwrap();
    write_mode(&run, Mode::Light).unwrap();
    std::fs::remove_dir_all(run.join("alpheus")).unwrap();
    let elsewhere = run.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, run.join("alpheus")).unwrap();
    assert!(write_mode(&run, Mode::Light).is_err(), "a symlinked directory refused");
    assert!(!elsewhere.join("mode").exists());
    let _ = std::fs::remove_dir_all(&run);
}

#[test]
fn a_stale_temporary_symlink_is_never_followed() {
    let run = std::env::temp_dir().join(format!("alpheus-{}-mode-stale", std::process::id()));
    let _ = std::fs::remove_dir_all(&run);
    std::fs::create_dir_all(&run).unwrap();
    write_mode(&run, Mode::Dark).unwrap();
    let victim = run.join("victim");
    std::fs::write(&victim, "keep").unwrap();
    std::os::unix::fs::symlink(&victim, run.join(format!("alpheus/.mode.{}", std::process::id()))).unwrap();
    write_mode(&run, Mode::Light).unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    assert_eq!(std::fs::read_to_string(run.join("alpheus/mode")).unwrap(), "light\n");
    let _ = std::fs::remove_dir_all(&run);
}

#[test]
fn a_failed_write_is_retried_and_logged_once_per_error() {
    use std::cell::Cell;
    let calls = Cell::new(0);
    let (mut written, mut error) = (None, None);
    let fail = |_: &Path, _: Mode| {
        calls.set(calls.get() + 1);
        Err(io::Error::other("disk"))
    };
    let run = Path::new("/run/x");
    let mut lines = 0;
    publish(Some(run), &mut written, &mut error, Mode::Dark, fail, |_| lines += 1);
    assert_eq!((written, calls.get(), lines), (None, 1, 1), "not marked written");
    publish(Some(run), &mut written, &mut error, Mode::Dark, fail, |_| lines += 1);
    assert_eq!((calls.get(), lines), (2, 1), "tried again, not logged again");
    let first = error.clone();
    assert!(first.is_some());
    publish(Some(run), &mut written, &mut error, Mode::Light, fail, |_| lines += 1);
    assert_eq!(lines, 1, "the same error on the next toggle: still one line");
    assert_eq!(error, first, "same error: same record, not re-logged");
    publish(Some(run), &mut written, &mut error, Mode::Light, |_, _| Ok(()), |_| lines += 1);
    assert_eq!((written, error.clone()), (Some(Mode::Light), None), "success marks and clears");
}

#[test]
fn no_runtime_directory_writes_nothing() {
    let (mut written, mut error) = (None, None);
    publish(None, &mut written, &mut error, Mode::Light, |_, _| panic!("no runtime: no write"), |_| panic!("nothing logged"));
    assert_eq!((written, error), (None, None));
}

#[test]
fn a_failed_write_is_an_error_never_a_panic() {
    let run = std::env::temp_dir().join(format!("alpheus-{}-mode-blocked", std::process::id()));
    let _ = std::fs::remove_dir_all(&run);
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("alpheus"), "a file where the directory should be").unwrap();
    assert!(write_mode(&run, Mode::Dark).is_err());
    let _ = std::fs::remove_dir_all(&run);
}
