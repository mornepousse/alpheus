use super::*;
use crate::notes::keys::NoteKey;
use crate::notes::model::Notes;
use crate::notes::text::CAP;
use alpheus_common::temp_path::TempPath;
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
fn load_reads_plain_text_as_is() {
    let t = TempPath::dir("notes-read");
    let dir = state_with(&t, "buy milk\n    café".as_bytes());
    assert_eq!(load(&dir), Loaded::Text("buy milk\n    café".to_string()));
}

/// M3: a file `clean` would change (edited outside the pad: CRLF, a tab, a control) opens read only,
/// shown cleaned, and is never silently rewritten.
#[test]
fn a_file_with_tabs_or_controls_opens_read_only_cleaned() {
    let t = TempPath::dir("notes-unclean");
    let dir = state_with(&t, "buy milk\r\n\tcafé\u{7}".as_bytes());
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotClean, "buy milk\n    café".to_string()));
    // Cleaning grows a tab to four spaces: what is shown is still capped.
    let dir = state_with(&t, "\t".repeat(CAP / 2).as_bytes());
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotClean, " ".repeat(CAP)));
}

/// A file too large or not UTF-8 shows its first CAP bytes read only (lossy, cleaned, a scalar
/// cut by the cap dropped whole).
#[test]
fn load_refuses_too_large_and_not_utf8_showing_their_start() {
    let t = TempPath::dir("notes-refuse");
    let dir = state_with(&t, &vec![b'a'; CAP + 1]);
    assert_eq!(load(&dir), Loaded::Refused(Refusal::TooLarge, "a".repeat(CAP)));
    let dir = state_with(&t, &[0xff, b'x', 0xfe]);
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotUtf8, "\u{fffd}x\u{fffd}".to_string()));
    let dir = state_with(&t, &vec![b'a'; CAP]);
    assert_eq!(load(&dir), Loaded::Text("a".repeat(CAP)));
    // A four-byte scalar straddles the cap (three of its bytes inside): dropped whole, never shown
    // as U+FFFD (which would fit exactly).
    let mut bytes = vec![b'a'; CAP - 3];
    bytes.extend_from_slice("😀 and more".as_bytes());
    let dir = state_with(&t, &bytes);
    assert_eq!(load(&dir), Loaded::Refused(Refusal::TooLarge, "a".repeat(CAP - 3)));
    // A large file edited elsewhere: its start shown cleaned.
    let mut bytes = b"x\r\ny\t".to_vec();
    bytes.extend(vec![b'z'; CAP]);
    let dir = state_with(&t, &bytes);
    let Loaded::Refused(Refusal::TooLarge, shown) = load(&dir) else { panic!("too large") };
    assert!(shown.starts_with("x\ny    zz"), "{:?}", shown.get(..12));
    assert!(shown.len() <= CAP);
}

#[test]
fn load_maps_a_file_that_is_not_ours_or_unreadable() {
    let t = TempPath::dir("notes-ours");
    let dir = state_with(&t, b"x");
    std::fs::remove_file(dir.join(FILE)).unwrap();
    std::fs::create_dir(dir.join(FILE)).unwrap();
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotOurs, String::new()), "nothing behind a refused file is shown");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotOurs, String::new()));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_dir(dir.join(FILE)).unwrap();
    std::fs::write(dir.join(FILE), b"x").unwrap();
    std::fs::set_permissions(dir.join(FILE), std::fs::Permissions::from_mode(0o000)).unwrap();
    let got = load(&dir);
    // A superuser reads it anyway: only the refusal's kind matters.
    assert!(matches!(&got, Loaded::Refused(Refusal::Unreadable, t) if t.is_empty()) || matches!(got, Loaded::Text(_)), "{got:?}");
}

/// Nothing behind a symlink is ever shown: a link named `notes.txt` to a readable file of ours is
/// refused with an empty pad.
#[test]
fn a_symlinked_file_shows_nothing() {
    let t = TempPath::dir("notes-link-read");
    let dir = state_with(&t, b"x");
    let target = t.join("elsewhere");
    std::fs::write(&target, b"private elsewhere").unwrap();
    std::fs::remove_file(dir.join(FILE)).unwrap();
    std::os::unix::fs::symlink(&target, dir.join(FILE)).unwrap();
    assert_eq!(load(&dir), Loaded::Refused(Refusal::NotOurs, String::new()));
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
    let Loaded::Refused(why, shown) = load(&dir) else { panic!("refused") };
    let mut pad = Notes::new(shown, Some(why));
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

/// A planted link named `notes.txt` is replaced, never followed: the decoy it points at keeps its
/// bytes and `notes.txt` becomes a regular file of ours, 0600.
#[test]
fn a_planted_symlink_is_replaced_never_followed() {
    let t = TempPath::dir("notes-symlink");
    let dir = state_with(&t, b"old");
    let decoy = t.join("decoy");
    std::fs::write(&decoy, b"decoy bytes").unwrap();
    std::fs::remove_file(dir.join(FILE)).unwrap();
    std::os::unix::fs::symlink(&decoy, dir.join(FILE)).unwrap();
    save(&dir, "fresh note").unwrap();
    assert_eq!(std::fs::read(&decoy).unwrap(), b"decoy bytes", "the decoy is untouched");
    let meta = std::fs::symlink_metadata(dir.join(FILE)).unwrap();
    assert!(meta.file_type().is_file(), "notes.txt is a regular file now, not a link");
    assert_eq!(mode(&dir.join(FILE)), 0o600);
    assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), b"fresh note");
}
