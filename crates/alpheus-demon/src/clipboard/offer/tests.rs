use super::*;
use std::time::Duration;

#[test]
fn sensitive_hint_wins_over_text() {
    assert_eq!(verdict(["text/plain", "x-kde-passwordManagerHint"], false), Verdict::Skip(Skip::Sensitive));
    for hint in SENSITIVE {
        assert_eq!(verdict(["UTF8_STRING", hint], false), Verdict::Skip(Skip::Sensitive), "{hint}");
    }
}

#[test]
fn own_marker_is_skipped() {
    assert_eq!(verdict(["text/plain", MARKER], false), Verdict::Skip(Skip::Own));
}

#[test]
fn quiet_skips_text() {
    assert_eq!(verdict(["text/plain"], true), Verdict::Skip(Skip::Quiet));
    assert_eq!(verdict(["text/plain"], false), Verdict::Read("text/plain"));
}

#[test]
fn text_order_prefers_utf8() {
    assert_eq!(verdict(["STRING", "text/plain", "UTF8_STRING"], false), Verdict::Read("UTF8_STRING"));
    assert_eq!(verdict(["text/plain;charset=utf-8", "UTF8_STRING"], false), Verdict::Read("text/plain;charset=utf-8"));
    assert_eq!(verdict(["TEXT"], false), Verdict::Read("TEXT"));
    assert_eq!(verdict(["image/png", "text/html"], false), Verdict::Skip(Skip::NoText));
    assert_eq!(verdict([], false), Verdict::Skip(Skip::NoText));
}

#[test]
fn the_password_quiet_state_follows_the_prompt() {
    let t = Instant::now();
    let mut q = PasswordQuiet::default();
    assert!(!q.active(t));
    q.opened();
    assert!(q.active(t + Duration::from_secs(60)), "open: quiet however long");
    q.closed(t);
    assert!(q.active(t + Duration::from_millis(4900)), "5 s after the close");
    assert!(!q.active(t + PASSWORD_QUIET));
    // A second prompt opened inside the window: quiet again until it closes.
    q.opened();
    assert!(q.active(t + Duration::from_secs(60)));
    q.closed(t + Duration::from_secs(60));
    assert!(q.active(t + Duration::from_secs(62)));
    assert!(!q.active(t + Duration::from_secs(66)));
}

#[test]
fn quiet_window_rules() {
    let now = Instant::now();
    let ago = |ms| now.checked_sub(Duration::from_millis(ms));
    assert!(in_quiet_window(true, None, now));
    assert!(in_quiet_window(false, ago(4900), now));
    assert!(!in_quiet_window(false, ago(5000), now));
    assert!(!in_quiet_window(false, None, now));
}

#[test]
fn buffer_caps_and_refuses_over_and_invalid() {
    let mut b = ReadBuffer::default();
    b.push(&[b'a'; MAX_BYTES]);
    assert!(!b.over());
    assert_eq!(b.finish().map(|s| s.len()), Some(MAX_BYTES));

    let mut b = ReadBuffer::default();
    let chunk = [b'a'; 1024];
    let mut stops = 0;
    for i in 0..5 {
        match step(Ok(1024), &chunk, &mut b) {
            Step::Stop => {
                stops = i + 1;
                break;
            }
            s => assert_eq!(s, Step::Continue),
        }
    }
    assert_eq!(stops, 5, "Stop on the chunk that passes the cap");
    assert!(b.over());
    assert_eq!(b.finish(), None);

    let mut b = ReadBuffer::default();
    b.push(&[0xff]);
    assert_eq!(b.finish(), None);
}

#[test]
fn step_waits_on_would_block_stops_at_eof_and_reports_errors() {
    let mut b = ReadBuffer::default();
    assert_eq!(step(Err(ErrorKind::WouldBlock.into()), &[], &mut b), Step::Continue);
    assert_eq!(step(Ok(0), &[], &mut b), Step::Stop);
    assert_eq!(step(Err(ErrorKind::BrokenPipe.into()), &[], &mut b), Step::Failed(ErrorKind::BrokenPipe));
}

#[test]
fn buffer_debug_has_no_bytes() {
    let mut b = ReadBuffer::default();
    b.push(b"buy milk");
    let s = format!("{b:?}");
    assert!(!s.contains("buy") && !s.contains("98") && !s.contains("109"), "{s}");
}

#[test]
fn buffer_keeps_one_byte_past_the_cap_and_no_more() {
    let mut b = ReadBuffer::default();
    b.push(&[b'a'; MAX_BYTES * 2]);
    assert_eq!(b.bytes.len(), MAX_BYTES + 1);
    assert!(b.over());
}

#[test]
fn verdict_order_is_sensitive_then_own_then_quiet() {
    assert_eq!(verdict([MARKER, "x-kde-passwordManagerHint", "text/plain"], true), Verdict::Skip(Skip::Sensitive));
    assert_eq!(verdict([MARKER, "text/plain"], true), Verdict::Skip(Skip::Own));
}

#[test]
fn the_buffer_never_reallocates() {
    let mut b = ReadBuffer::default();
    b.push(b"a");
    let cap = b.bytes.capacity();
    assert!(cap > MAX_BYTES, "room for the cap and the one byte past it from the first push");
    for _ in 0..10 {
        b.push(&[b'a'; 1024]);
        assert_eq!(b.bytes.capacity(), cap);
    }
}
