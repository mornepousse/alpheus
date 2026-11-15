use super::*;
use crate::notes::keys::NoteKey;
use crate::notes::model::Notes;
use crate::notes::text::CAP;
use alpheus_commun::temp_path::TempPath;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// A state directory (0700, as `write_private` makes it) holding `bytes` as the notes file.
fn state_with(t: &TempPath, bytes: &[u8]) -> PathBuf {
    let dir = t.join("alpheus");
    crate::state_file::write_private(&dir, FILE, bytes).unwrap();
    dir
}

#[test]
fn load_absent_is_an_empty_pad() {
    let t = TempPath::dir("notes-absent");
    assert_eq!(load(&t.join("alpheus")), Loaded::Text(String::new()));
}

#[test]
fn load_reads_utf8_text_cleaned() {
    let t = TempPath::dir("notes-read");
    let dir = state_with(&t, "buy milk\r\n\tcafé\u{7}".as_bytes());
    assert_eq!(load(&dir), Loaded::Text("buy milk\n    café".to_string()), "what Pad::new is fed is clean");
}

#[test]
fn load_refuses_too_large_and_not_utf8() {
    let t = TempPath::dir("notes-refuse");
    let dir = state_with(&t, &vec![b'a'; CAP + 1]);
    assert_eq!(load(&dir), Loaded::Refused(Refusal::TooLarge));
    let dir = state_with(&t, &[0xff, 0xfe]);
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotUtf8));
    let dir = state_with(&t, &vec![b'a'; CAP]);
    assert_eq!(load(&dir), Loaded::Text("a".repeat(CAP)));
    // Cleaning grows a tab to four spaces: a file that fits as bytes but not as a pad is refused.
    let dir = state_with(&t, "\t".repeat(CAP / 2).as_bytes());
    assert_eq!(load(&dir), Loaded::Refused(Refusal::TooLarge));
}

#[test]
fn load_maps_a_file_that_is_not_ours_or_unreadable() {
    let t = TempPath::dir("notes-ours");
    let dir = state_with(&t, b"x");
    std::fs::remove_file(dir.join(FILE)).unwrap();
    std::fs::create_dir(dir.join(FILE)).unwrap();
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotOurs));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotOurs));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_dir(dir.join(FILE)).unwrap();
    std::fs::write(dir.join(FILE), b"x").unwrap();
    std::fs::set_permissions(dir.join(FILE), std::fs::Permissions::from_mode(0o000)).unwrap();
    let got = load(&dir);
    // A superuser reads it anyway: only the refusal's kind matters.
    assert!(matches!(got, Loaded::Refused(Refusal::Unreadable) | Loaded::Text(_)), "{got:?}");
}

#[test]
fn save_is_private_and_atomic() {
    let t = TempPath::new("notes-save");
    let dir = t.join("alpheus");
    save(&dir, "buy milk").unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join(FILE)), 0o600);
    assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), "buy milk");
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, [std::ffi::OsString::from(FILE)], "no .notes.txt.<pid> left behind");
}

#[test]
fn an_empty_pad_removes_the_file() {
    let t = TempPath::dir("notes-empty");
    let dir = state_with(&t, b"old");
    save(&dir, "").unwrap();
    assert!(!dir.join(FILE).exists());
}

#[test]
fn saving_empty_with_no_file_is_ok() {
    let t = TempPath::dir("notes-empty-none");
    assert!(save(&t.join("alpheus"), "").is_ok());
    let dir = t.join("alpheus");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(save(&dir, "").is_ok());
}

/// The contract the app keeps: a refused file builds a read only pad, which never turns dirty, so
/// the app never calls `save` and the file stays as it was.
#[test]
fn a_refused_file_is_never_written() {
    let t = TempPath::dir("notes-refused");
    let bytes = vec![0xff_u8, 0xfe, b'x'];
    let dir = state_with(&t, &bytes);
    let Loaded::Refused(why) = load(&dir) else { panic!("refused") };
    let mut pad = Notes::new(String::new(), Some(why));
    for c in "typing".chars() {
        pad.key(NoteKey::Char(c));
    }
    assert!(!pad.dirty, "a read only pad stays clean");
    assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), bytes);
}

#[test]
fn the_save_deadline_exists_only_while_open_and_dirty() {
    let t = Instant::now();
    assert_eq!(save_deadline(true, None), None);
    assert_eq!(save_deadline(false, Some(t)), None);
    assert_eq!(save_deadline(true, Some(t)), Some(t + SAVE_AFTER));
}
