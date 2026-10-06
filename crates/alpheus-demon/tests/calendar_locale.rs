//! The calendar's first day of the week read from REAL locales (glibc's
//! locale archive, `LOCALE_ARCHIVE` — inherited from the host, the dev shell
//! sets none), each in a child process with its own `LC_ALL` — never this
//! process's environment, never the session's locale. Skipped with a message
//! when no archive is there; a locale the archive lacks (`locale -a`) is
//! skipped with a message, never faked.
//! `nice -n 19 nix develop -c env CARGO_BUILD_JOBS=4 cargo test -p alpheus-demon --test calendar_locale -- --ignored`

use alpheus_demon::calendar::{MONDAY, locale_first_day};
use std::ffi::OsStr;
use std::process::Command;

const SUNDAY: u8 = 0;

/// `fr_FR.UTF-8` and `locale -a`'s `fr_FR.utf8` name the same locale (glibc
/// normalises the codeset).
fn normalized(name: &str) -> String {
    name.to_lowercase().replace('-', "")
}

/// The locales `archive` holds, as `locale -a` lists them there (normalized);
/// None when `locale` cannot run.
fn installed(archive: &OsStr) -> Option<Vec<String>> {
    let out =
        Command::new("locale").arg("-a").env_clear().env("PATH", std::env::var_os("PATH")?).env("LOCALE_ARCHIVE", archive).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().map(normalized).collect())
}

/// Run by `locale_first_day_reads_lc_time` in a child process: prints what
/// `locale_first_day` reads there.
#[test]
#[ignore = "a child of locale_first_day_reads_lc_time"]
fn locale_first_day_probe() {
    if std::env::var_os("ALPHEUS_CALENDAR_PROBE").is_some() {
        println!("first_day={}", locale_first_day());
    }
}

#[test]
#[ignore = "real locales through LOCALE_ARCHIVE (nix develop)"]
fn locale_first_day_reads_lc_time() {
    let Some(archive) = std::env::var_os("LOCALE_ARCHIVE").filter(|a| std::path::Path::new(a).exists()) else {
        eprintln!("skipped: no LOCALE_ARCHIVE to read real locales from");
        return;
    };
    let Some(installed) = installed(&archive) else {
        eprintln!("skipped: `locale -a` cannot list this archive's locales");
        return;
    };
    let first_day = |lc_all: &str| {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "locale_first_day_probe", "--ignored", "--nocapture", "--test-threads=1"])
            .env_clear()
            .env("LOCALE_ARCHIVE", &archive)
            .env("LC_ALL", lc_all)
            .env("ALPHEUS_CALENDAR_PROBE", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        // libtest prints it after "test … ... " on the same line.
        let day = text.split("first_day=").nth(1).and_then(|rest| rest.get(..1)).and_then(|d| d.parse::<u8>().ok());
        day.unwrap_or_else(|| panic!("no answer from the probe: {text}"))
    };
    // The host's archive decides what is there: a locale it lacks is skipped.
    for (lc_all, expected, why) in [
        ("fr_FR.UTF-8", MONDAY, "Mae's LC_TIME"),
        ("en_US.UTF-8", SUNDAY, "en_US"),
        ("C.UTF-8", MONDAY, "C.UTF-8 says Sunday by saying nothing"),
    ] {
        if !installed.contains(&normalized(lc_all)) {
            eprintln!("skipped {lc_all}: not in this archive");
            continue;
        }
        assert_eq!(first_day(lc_all), expected, "{lc_all}: {why}");
    }
    // Built into glibc: always there.
    assert_eq!(first_day("C"), MONDAY, "C says Sunday by saying nothing (question 4)");
    // The unreadable case: a locale no archive holds.
    assert!(!installed.contains(&normalized("xx_YY.UTF-8")), "xx_YY installed? {installed:?}");
    assert_eq!(first_day("xx_YY.UTF-8"), MONDAY, "not installed: unreadable");
}
