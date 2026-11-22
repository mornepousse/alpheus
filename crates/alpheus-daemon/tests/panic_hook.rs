//! The daemon's panic hook (`log::install_panic_hook`), for real: this test runs itself again in a
//! child process, which installs the hook and panics in a named thread with a fictional secret; the
//! child's standard error must hold the thread and the place, never the secret (a panic message can
//! quote private text: the quick notes, a password prompt). Its own test binary, so the hook never
//! touches another test's panics.

use std::process::Command;

const CHILD: &str = "ALPHEUS_PANIC_HOOK_CHILD";

#[test]
fn the_installed_hook_logs_the_place_never_the_payload() {
    if std::env::var_os(CHILD).is_some() {
        alpheus_daemon::log::install_panic_hook();
        let joined = std::thread::Builder::new()
            .name("hook-probe".into())
            .spawn(|| panic!("byte index 3 is not a char boundary of `secret zq note`"))
            .unwrap()
            .join();
        assert!(joined.is_err(), "the probe thread panicked");
        // A panic caught by `catch_unwind` (the notes' and the launcher's guards) still unwinds.
        assert!(std::panic::catch_unwind(|| panic!("caught qz")).is_err());
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "the_installed_hook_logs_the_place_never_the_payload", "--nocapture"])
        .env(CHILD, "1")
        .env_remove("RUST_BACKTRACE")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the child test failed: {err}");
    assert!(err.contains("alpheus-demon: thread 'hook-probe' panicked at "), "{err}");
    assert!(err.contains("tests/panic_hook.rs:"), "the place: {err}");
    assert_eq!(err.matches("panicked at").count(), 2, "both panics logged, the caught one too: {err}");
    for secret in ["secret", "zq", "qz"] {
        assert!(!err.contains(secret), "the payload reached the log: {err}");
    }
}
