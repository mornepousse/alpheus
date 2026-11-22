use super::*;

const LINE: &str = r#"12: block id 1234:5678 serial "S1" name "Test Stick" hash "h" parent-hash "p" via-port "1-2" with-interface 08:06:50 with-connect-type "hotplug""#;

fn one(line: &str) -> Option<UsbDevice> {
    parse_blocked(line.as_bytes()).devices.into_iter().next()
}

#[test]
fn a_usbguard_line_is_read() {
    assert_eq!(one(LINE), Some(UsbDevice { id: 12, vid_pid: "1234:5678".into(), name: "Test Stick".into() }));
}

#[test]
fn only_block_lines_count() {
    let out = format!("{}\nheader\n\n3: allow id abcd:0001 name \"Test Keyboard\"\ngarbage: block\n{LINE}\n", "");
    let p = parse_blocked(out.as_bytes());
    assert_eq!(p.devices.len(), 1);
    assert_eq!(p.devices[0].id, 12);
}

#[test]
fn a_serial_cannot_pose_as_the_name() {
    let d = one(r#"7: block id abcd:0001 serial "x\" name \"Fake" name "Test Keyboard" hash "h""#).unwrap();
    assert_eq!(d.name, "Test Keyboard");
    // And with no real name key the serial's text is never taken for one.
    let d = one(r#"7: block id abcd:0001 serial "x\" name \"Fake" hash "h""#).unwrap();
    assert_eq!(d.name, "?");
}

#[test]
fn escapes_are_decoded_only_for_quote_and_backslash() {
    let d = one(r#"7: block id abcd:0001 name "a\\b \"q\" \x07""#).unwrap();
    assert_eq!(d.name, r#"a\b "q" \x07"#);
    assert!(!d.name.chars().any(|c| c.is_control()));
}

#[test]
fn a_hostile_name_is_cleaned_cut_and_never_empty() {
    let d = one("7: block id abcd:0001 name \"a\u{202e}b\u{7}c\u{200b}d\"").unwrap();
    assert_eq!(d.name, "abcd");
    let long = "z".repeat(300);
    let d = one(&format!("7: block id abcd:0001 name \"{long}\"")).unwrap();
    assert_eq!(d.name.chars().count(), NAME_CHARS);
    assert_eq!(one("7: block id abcd:0001 name \"\"").unwrap().name, "?");
    assert_eq!(one("7: block id abcd:0001 name \"\u{202e}\u{7}\"").unwrap().name, "?");
    assert_eq!(one("7: block id abcd:0001 hash \"h\"").unwrap().name, "?");
}

#[test]
fn ids_and_vid_pid_are_checked() {
    assert!(one(r#"99999999999: block id 1234:5678 name "A""#).is_none(), "past u32 and 10 digits");
    assert!(one(r#"4294967296: block id 1234:5678 name "A""#).is_none(), "10 digits past u32");
    assert!(one(r#"12: block id 12:5678 name "A""#).is_none());
    assert!(one(r#"12: block id ABCD:5678 name "A""#).is_none(), "upper case");
    assert!(one(r#"12: block id 1234:5678x name "A""#).is_none());
    assert!(one(r#"00000000012: block id 1234:5678 name "A""#).is_none(), "11 digits, though it fits");
    assert!(one(r#"12: block id gggg:5678 name "A""#).is_none(), "not hex");
    assert!(one(r#"-1: block id 1234:5678 name "A""#).is_none());
    assert_eq!(one(r#"4294967295: block id 1234:5678 name "A""#).unwrap().id, u32::MAX);
}

#[test]
fn invalid_utf8_is_lossy_not_fatal() {
    let mut out = b"7: block id abcd:0001 name \"A".to_vec();
    out.push(0xff);
    out.extend_from_slice(b"B\"\n");
    let p = parse_blocked(&out);
    assert_eq!(p.devices.len(), 1);
}

#[test]
fn at_most_64_devices() {
    let out: String = (0..100).map(|i| format!("{i}: block id 1234:5678 name \"D{i}\"\n")).collect();
    let p = parse_blocked(out.as_bytes());
    assert_eq!(p.devices.len(), DEVICES_MAX);
    assert!(p.more);
    let out: String = (0..64).map(|i| format!("{i}: block id 1234:5678 name \"D{i}\"\n")).collect();
    let p = parse_blocked(out.as_bytes());
    assert_eq!((p.devices.len(), p.more), (64, false));
}

#[test]
fn allow_takes_the_id_as_a_whole_element() {
    let allow: Vec<String> = ["pkexec", "/x/helper", "allow", "{id}"].iter().map(|s| s.to_string()).collect();
    assert_eq!(allow_argv(&allow, 7).unwrap(), ["pkexec", "/x/helper", "allow", "7"]);
    assert_eq!(allow_argv(&[], 7), None);
}

#[test]
fn the_picker_line_has_the_name_and_ids_but_not_the_device_id() {
    let d = UsbDevice { id: 12, vid_pid: "1234:5678".into(), name: "Test Stick".into() };
    assert_eq!(picker_line(&d), "1234:5678  ·  Test Stick");
    // A name cannot fake the ids: the real ones lead the row.
    let fake = UsbDevice { id: 13, vid_pid: "dead:beef".into(), name: "Evil (1234:5678)".into() };
    assert!(picker_line(&fake).starts_with("dead:beef  ·  "));
}
