use super::*;

fn requests() -> Vec<Request> {
    vec![
        Request::Toggle,
        Request::Close,
        Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() },
        Request::Dmenu { prompt: Some(String::new()), lines: vec!["one".into()], flags: DmenuFlags::default() },
        Request::Dmenu {
            prompt: Some("Target (IP / host): ".into()),
            lines: vec!["a b c".into(), "tab\there".into(), "cr\rinside".into(), "café ☕ 日本".into(), "  padded  ".into()],
            flags: DmenuFlags::default(),
        },
        Request::Dmenu {
            prompt: None,
            lines: vec!["a".into(), "b".into()],
            flags: DmenuFlags { index: true, password: false, visible: None },
        },
        Request::Dmenu { prompt: Some("Key: ".into()), lines: vec![], flags: DmenuFlags { index: false, password: true, visible: None } },
        Request::Dmenu {
            prompt: Some("x".into()),
            lines: vec!["l".into()],
            flags: DmenuFlags { index: true, password: true, visible: None },
        },
        Request::Dmenu {
            prompt: None,
            lines: vec!["a".into(), "b".into(), "c".into()],
            flags: DmenuFlags { index: false, password: false, visible: Some(2) },
        },
        Request::Dmenu {
            prompt: Some("Go".into()),
            lines: vec!["a".into()],
            flags: DmenuFlags { index: true, password: false, visible: Some(0) },
        },
        Request::Dnd(DndVerb::On),
        Request::Dnd(DndVerb::Off),
        Request::Dnd(DndVerb::Toggle),
        Request::DndOn(DndEnd::For(1800)),
        Request::DndOn(DndEnd::Until(1_763_229_600)),
        Request::ClearNotifications,
        Request::Osd(OsdVerb::Brightness),
        Request::Quick(QuickVerb::Top),
        Request::Quick(QuickVerb::Sound),
        Request::Quick(QuickVerb::Wifi),
        Request::Quick(QuickVerb::Bluetooth),
        Request::Quick(QuickVerb::Vpn),
        Request::Calc,
        Request::Outputs,
        Request::OutputsTry(String::new()),
        Request::OutputsTry("step\tDP-1\t1920x1080@60 0 0 1000 0\nstep\tHDMI-A-1\toff\n".into()),
        Request::OutputsTry("café ☕ 日本\n".into()),
        Request::OutputsKeep,
        Request::OutputsRevert,
    ]
}

fn decode(bytes: &[u8]) -> Result<Option<Request>, IpcError> {
    RequestDecoder::new().feed(bytes)
}

#[test]
fn every_request_round_trips() {
    for r in requests() {
        assert_eq!(decode(&encode_request(&r)), Ok(Some(r.clone())), "{r:?}");
    }
}

#[test]
fn byte_by_byte_gives_the_same_request() {
    for r in requests() {
        let bytes = encode_request(&r);
        let mut d = RequestDecoder::new();
        for (i, b) in bytes.iter().enumerate() {
            let got = d.feed(std::slice::from_ref(b)).unwrap();
            if i + 1 < bytes.len() {
                assert_eq!(got, None, "{r:?} complete too early, at byte {i}");
            } else {
                assert_eq!(got, Some(r.clone()));
            }
        }
    }
}

#[test]
fn the_wire_format_is_the_documented_one() {
    assert_eq!(encode_request(&Request::Toggle), b"alpheus-launcher/1 toggle\n");
    assert_eq!(encode_request(&Request::Close), b"alpheus-launcher/1 close\n");
    assert_eq!(encode_request(&Request::Dnd(DndVerb::On)), b"alpheus-launcher/1 dnd on\n");
    assert_eq!(encode_request(&Request::Dnd(DndVerb::Off)), b"alpheus-launcher/1 dnd off\n");
    assert_eq!(encode_request(&Request::Dnd(DndVerb::Toggle)), b"alpheus-launcher/1 dnd toggle\n");
    assert_eq!(encode_request(&Request::ClearNotifications), b"alpheus-launcher/1 clear-notifications\n");
    assert_eq!(encode_request(&Request::Osd(OsdVerb::Brightness)), b"alpheus-launcher/1 osd brightness\n");
    assert_eq!(encode_request(&Request::Quick(QuickVerb::Top)), b"alpheus-launcher/1 quick\n");
    assert_eq!(encode_request(&Request::Quick(QuickVerb::Sound)), b"alpheus-launcher/1 quick sound\n");
    assert_eq!(encode_request(&Request::Quick(QuickVerb::Wifi)), b"alpheus-launcher/1 quick wifi\n");
    assert_eq!(encode_request(&Request::Quick(QuickVerb::Bluetooth)), b"alpheus-launcher/1 quick bluetooth\n");
    assert_eq!(encode_request(&Request::Quick(QuickVerb::Vpn)), b"alpheus-launcher/1 quick vpn\n");
    assert_eq!(encode_request(&Request::Calc), b"alpheus-launcher/1 calc\n");
    assert_eq!(
        encode_request(&Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }),
        b"alpheus-launcher/1 dmenu - 0\n"
    );
    let dmenu = Request::Dmenu { prompt: Some("Go: ".into()), lines: vec!["a".into(), "bé".into()], flags: DmenuFlags::default() };
    assert_eq!(encode_request(&dmenu), "alpheus-launcher/1 dmenu 4 5\nGo: a\nbé".as_bytes());
    let flagged =
        |index, password| Request::Dmenu { prompt: None, lines: vec!["a".into()], flags: DmenuFlags { index, password, visible: None } };
    assert_eq!(encode_request(&flagged(true, false)), b"alpheus-launcher/1 dmenu - 1 index\na");
    assert_eq!(encode_request(&flagged(false, true)), b"alpheus-launcher/1 dmenu - 1 password\na");
    assert_eq!(encode_request(&flagged(true, true)), b"alpheus-launcher/1 dmenu - 1 index password\na");
    let visible =
        |index, visible| Request::Dmenu { prompt: None, lines: vec!["a".into()], flags: DmenuFlags { index, password: false, visible } };
    assert_eq!(encode_request(&visible(false, Some(3))), b"alpheus-launcher/1 dmenu - 1 visible 3\na");
    assert_eq!(encode_request(&visible(true, Some(0))), b"alpheus-launcher/1 dmenu - 1 index visible 0\na");
    assert_eq!(encode_reply(&Reply::Index(Some(2))), b"index 2\n");
    assert_eq!(encode_reply(&Reply::Index(None)), b"index -\n");
    assert_eq!(encode_reply(&Reply::Done), b"done\n");
    assert_eq!(encode_reply(&Reply::Chosen("bé".into())), "chosen 3\nbé".as_bytes());
    assert_eq!(encode_reply(&Reply::Cancelled), b"cancelled\n");
    assert_eq!(encode_reply(&Reply::Busy), b"busy\n");
    assert_eq!(encode_reply(&Reply::Error("no".into())), b"error no\n");
}

#[test]
fn malformed_headers_are_refused() {
    for h in [
        &b"toggle\n"[..],
        b"alpheus-launcher/2 toggle\n",
        b"alpheus-launcher/1 open\n",
        b"alpheus-launcher/1\n",
        b"alpheus-launcher/1 toggle now\n",
        b"alpheus-launcher/1 calc now\n",
        b"alpheus-launcher/1  toggle\n",
        b"alpheus-launcher/1 dmenu x 3\n",
        b"alpheus-launcher/1 dmenu - -1\n",
        b"alpheus-launcher/1 dmenu - +3\n",
        b"alpheus-launcher/1 dmenu -\n",
        b"alpheus-launcher/1 dmenu - 1 2\n",
        b"\n",
        b"alpheus-launcher/1 \xff\n",
        b"alpheus-launcher/1 dnd\n",
        b"alpheus-launcher/1 dnd maybe\n",
        b"alpheus-launcher/1 dnd on now\n",
        b"alpheus-launcher/1 dnd  on\n",
        b"alpheus-launcher/1 clear-notifications now\n",
        b"alpheus-launcher/1 osd\n",
        b"alpheus-launcher/1 osd volume\n",
        b"alpheus-launcher/1 osd brightness now\n",
        b"alpheus-launcher/1 osd  brightness\n",
        b"alpheus-launcher/1 quick volume\n",
        b"alpheus-launcher/1 quick sound now\n",
        b"alpheus-launcher/1 quick Sound\n",
        b"alpheus-launcher/1 quick Wifi\n",
        b"alpheus-launcher/1 quick wifi now\n",
        b"alpheus-launcher/1 dmenu - 0 indexes\n",
        b"alpheus-launcher/1 dmenu - 0 index index\n",
        b"alpheus-launcher/1 dmenu - 0 visible\n",
        b"alpheus-launcher/1 dmenu - 0 visible x\n",
        b"alpheus-launcher/1 dmenu - 0 visible -1\n",
        b"alpheus-launcher/1 dmenu - 0 visible 1234567890123\n",
        b"alpheus-launcher/1 dmenu - 0 visible 1 visible 2\n",
        b"alpheus-launcher/1 dmenu - 0 visible 1 index\n",
        b"alpheus-launcher/1 dmenu - 0 password password\n",
        b"alpheus-launcher/1 dmenu - 0 password index\n",
        b"alpheus-launcher/1 dmenu - 0  index\n",
        b"alpheus-launcher/1 dmenu - 0 index \n",
        b"alpheus-launcher/1 dmenu - 0 Index\n",
    ] {
        assert!(matches!(decode(h), Err(IpcError::Malformed(_))), "{:?} → {:?}", String::from_utf8_lossy(h), decode(h));
    }
}

/// A request from a client that knows no flags (the trailing words are optional) still reads,
/// as no flag: the header stays `alpheus-launcher/1`.
#[test]
fn a_dmenu_header_without_flags_still_decodes() {
    assert_eq!(
        decode(b"alpheus-launcher/1 dmenu - 3\na\nb"),
        Ok(Some(Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into()], flags: DmenuFlags::default() }))
    );
}

#[test]
fn bytes_after_a_whole_request_are_malformed() {
    assert!(matches!(decode(b"alpheus-launcher/1 toggle\nx"), Err(IpcError::Malformed(_))));
    assert!(matches!(decode(b"alpheus-launcher/1 dmenu - 1\nab"), Err(IpcError::Malformed(_))));
    assert!(matches!(decode(b"alpheus-launcher/1 dnd on\nx"), Err(IpcError::Malformed(_))));
    assert!(matches!(decode(b"alpheus-launcher/1 clear-notifications\nx"), Err(IpcError::Malformed(_))));
    assert!(matches!(decode(b"alpheus-launcher/1 osd brightness\nx"), Err(IpcError::Malformed(_))));
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(b"alpheus-launcher/1 close\n"), Ok(Some(Request::Close)));
    assert!(matches!(d.feed(b"x"), Err(IpcError::Malformed(_))), "a second message on the same connection");
}

#[test]
fn every_dnd_word_parses_back() {
    for v in DndVerb::ALL {
        assert_eq!(DndVerb::parse(v.word()), Some(v));
    }
    assert_eq!(DndVerb::parse("On"), None, "the words are exact");
    assert_eq!(DndVerb::parse(""), None);
}

#[test]
fn every_osd_word_parses_back() {
    for v in OsdVerb::ALL {
        assert_eq!(OsdVerb::parse(v.word()), Some(v));
    }
    assert_eq!(OsdVerb::parse("Brightness"), None, "the words are exact");
    assert_eq!(OsdVerb::parse("volume"), None, "volume is passive: no verb");
}

#[test]
fn every_quick_word_parses_back() {
    for v in QuickVerb::ALL {
        if let Some(w) = v.word() {
            assert_eq!(QuickVerb::parse(w), Some(v));
        }
    }
    assert_eq!(QuickVerb::Top.word(), None, "the bare verb");
    assert_eq!(QuickVerb::parse("Sound"), None, "the words are exact");
    assert_eq!(QuickVerb::parse("wifi"), Some(QuickVerb::Wifi), "quick Wi-Fi spec § 1");
    assert_eq!(QuickVerb::parse("bluetooth"), Some(QuickVerb::Bluetooth), "quick Bluetooth spec § 1");
    assert_eq!(QuickVerb::parse("vpn"), Some(QuickVerb::Vpn), "quick VPN spec § 1");
    assert_eq!(QuickVerb::ALL.len(), 5);
    assert_eq!(QuickVerb::parse("network"), None, "the Wi-Fi's word is wifi");
}

#[test]
fn a_header_without_end_is_refused_at_max_header() {
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(&[b'a'; MAX_HEADER]), Ok(None), "MAX_HEADER bytes may still be followed by '\\n'");
    assert_eq!(d.feed(b"a"), Err(IpcError::TooLarge("header")));
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(&vec![b'a'; 1 << 20]), Err(IpcError::TooLarge("header")));
    assert!(d.buffered() <= MAX_HEADER + 1, "{} bytes buffered", d.buffered());
}

#[test]
fn prompt_and_body_limits_are_checked_on_the_header() {
    let mut chunk = format!("{MAGIC} dmenu {} 0\n", MAX_PROMPT + 1).into_bytes();
    chunk.extend(vec![b'p'; 4096]);
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(&chunk), Err(IpcError::TooLarge("prompt")));
    assert!(d.buffered() <= MAX_HEADER, "{} bytes buffered", d.buffered());
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(format!("{MAGIC} dmenu - {}\n", MAX_DMENU_BYTES + 1).as_bytes()), Err(IpcError::TooLarge("dmenu input")));
    assert!(d.buffered() <= MAX_HEADER, "{} bytes buffered", d.buffered());
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(format!("{MAGIC} dmenu {MAX_PROMPT} {MAX_DMENU_BYTES}\n").as_bytes()), Ok(None), "the limits themselves pass");
}

#[test]
fn too_many_lines_are_refused_as_they_arrive() {
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(format!("{MAGIC} dmenu - {MAX_DMENU_BYTES}\n").as_bytes()), Ok(None));
    let chunk = b"x\n".repeat(1000);
    let mut result = Ok(None);
    for _ in 0..MAX_LINES / 1000 {
        result = d.feed(&chunk);
        if result.is_err() {
            break;
        }
    }
    assert_eq!(result, Err(IpcError::TooLarge("lines")));
    assert!(d.buffered() < 2 * MAX_LINES, "refused long before the 4 MiB body: {} bytes buffered", d.buffered());
}

#[test]
fn the_body_is_buffered_once_at_its_announced_size() {
    let (prompt, body) = ("Go: ", "x".repeat(1_000_000));
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(format!("{MAGIC} dmenu {} {}\n", prompt.len(), body.len()).as_bytes()), Ok(None));
    let mut rest = format!("{prompt}{body}").into_bytes();
    let last = rest.split_off(rest.len() - 1);
    for chunk in rest.chunks(64 * 1024) {
        assert_eq!(d.feed(chunk), Ok(None));
    }
    assert_eq!(d.buf.capacity(), prompt.len() + body.len(), "reserved once, never doubled");
    assert_eq!(d.feed(&last), Ok(Some(Request::Dmenu { prompt: Some(prompt.into()), lines: vec![body], flags: DmenuFlags::default() })));
}

#[test]
fn exactly_max_lines_is_accepted() {
    let r = Request::Dmenu { prompt: None, lines: (0..MAX_LINES).map(|i| (i % 10).to_string()).collect(), flags: DmenuFlags::default() };
    assert_eq!(decode(&encode_request(&r)), Ok(Some(r)));
}

#[test]
fn non_utf8_prompt_or_body_is_malformed() {
    let mut p = format!("{MAGIC} dmenu 1 1\n").into_bytes();
    p.extend([0xff, b'a']);
    assert!(matches!(decode(&p), Err(IpcError::Malformed(_))));
    let mut b = format!("{MAGIC} dmenu - 1\n").into_bytes();
    b.push(0xff);
    assert!(matches!(decode(&b), Err(IpcError::Malformed(_))));
}

#[test]
fn every_reply_round_trips() {
    for r in [
        Reply::Done,
        Reply::Cancelled,
        Reply::Busy,
        Reply::Chosen(String::new()),
        Reply::Chosen("ssh  host-1 → café".into()),
        Reply::Chosen("two\nlines".into()),
        Reply::Error("launcher closed".into()),
        Reply::Error(String::new()),
        Reply::Index(Some(0)),
        Reply::Index(Some(MAX_LINES - 1)),
        Reply::Index(None),
    ] {
        assert_eq!(decode_reply(&encode_reply(&r)), Ok(r.clone()), "{r:?}");
    }
}

#[test]
fn error_text_is_one_bounded_line() {
    assert_eq!(encode_reply(&Reply::Error("bad\nthing\r".into())), b"error bad thing \n");
    let Ok(Reply::Error(t)) = decode_reply(&encode_reply(&Reply::Error("é".repeat(MAX_ERROR)))) else { panic!("not an error reply") };
    assert!(t.len() <= MAX_ERROR && t.len() >= MAX_ERROR - 1 && t.chars().all(|c| c == 'é'), "{} bytes", t.len());
}

#[test]
fn cut_or_odd_replies_are_refused() {
    assert_eq!(decode_reply(b""), Err(IpcError::Truncated));
    assert_eq!(decode_reply(b"done"), Err(IpcError::Truncated));
    assert_eq!(decode_reply(b"chosen 5\nab"), Err(IpcError::Truncated));
    for bad in [
        &b"chosen 1\nab"[..],
        b"done\nx",
        b"chosen x\n",
        b"hello\n",
        b"chosen\n",
        b"\xff\n",
        b"index\n",
        b"index x\n",
        b"index -1\n",
        b"index 1\nx",
        b"index  1\n",
    ] {
        assert!(matches!(decode_reply(bad), Err(IpcError::Malformed(_))), "{:?}", String::from_utf8_lossy(bad));
    }
    assert_eq!(decode_reply(format!("chosen {}\n", MAX_DMENU_BYTES + 1).as_bytes()), Err(IpcError::TooLarge("chosen line")));
    assert_eq!(decode_reply(format!("index {MAX_LINES}\n").as_bytes()), Err(IpcError::TooLarge("index")), "past the last line");
}

#[test]
fn split_lines_follows_fuzzel() {
    assert_eq!(split_lines("a\nb\n"), ["a", "b"]);
    assert_eq!(split_lines("a\r\nb\r\n"), ["a", "b"]);
    assert_eq!(split_lines("\n\n a \n\r\n"), [" a "]);
    assert_eq!(split_lines("x\r\r\n"), ["x\r"], "one '\\r' stripped, not all");
    assert!(split_lines("").is_empty());
    assert_eq!(numbered_lines("a\n\nb").collect::<Vec<_>>(), [(0, "a"), (2, "b")]);
    assert_eq!(numbered_lines("\r\n\nx\r\n").collect::<Vec<_>>(), [(2, "x")], "\"\\r\" alone is an empty line too");
}

/// A password's bytes are overwritten before they are freed, spare capacity included (what a
/// Backspace popped, what a push left behind).
#[test]
fn zeroed_overwrites_the_whole_allocation() {
    let mut v = b"hunter2".to_vec();
    v.reserve(9);
    v.truncate(3);
    let capacity = v.capacity();
    zero(&mut v);
    assert_eq!(v.len(), capacity, "the spare capacity is written too");
    assert!(v.iter().all(|&b| b == 0), "{v:?}");
    let mut s = String::from("hunter2");
    s.pop();
    wipe(&mut s);
    assert!(s.is_empty());
}

#[test]
fn socket_path_joins_the_name() {
    assert_eq!(socket_path(Path::new("/run/user/1000")), Path::new("/run/user/1000/alpheus-launcher.sock"));
}

#[test]
fn the_decoder_is_poisoned_after_a_malformed_header() {
    let mut d = RequestDecoder::new();
    assert!(matches!(d.feed(b"alpheus-launcher/1\n"), Err(IpcError::Malformed(_))));
    // The second call completes what would otherwise be a valid "toggle" request
    // if the decoder kept going; it must not, because the connection already
    // sent something malformed.
    assert!(!matches!(d.feed(b" toggle\n"), Ok(Some(Request::Toggle))), "poisoned decoder must not yield a request");
    assert!(d.feed(b" toggle\n").is_err(), "poisoned decoder keeps refusing");
}

#[test]
fn the_decoder_is_poisoned_after_a_dmenu_overflow() {
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(format!("{MAGIC} dmenu - 1\n").as_bytes()), Ok(None));
    assert!(matches!(d.feed(b"ab"), Err(IpcError::Malformed(_))), "bytes after the request");
    assert!(d.feed(b"c").is_err(), "poisoned decoder keeps refusing");
    assert!(d.feed(&[]).is_err(), "poisoned decoder refuses even an empty feed");
}

#[test]
fn empty_dmenu_lines_are_dropped() {
    // The dmenu rule: a line holding nothing is not a line.
    let r = Request::Dmenu { prompt: None, lines: vec![String::new()], flags: DmenuFlags::default() };
    assert_eq!(
        decode(&encode_request(&r)),
        Ok(Some(Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() })),
        "vec![\"\"] round-trips to vec![]"
    );

    // A hostile body with blank lines in the middle: fed directly, bypassing encode_request.
    let body = "a\n\nb\n";
    let mut d = RequestDecoder::new();
    assert_eq!(d.feed(format!("{MAGIC} dmenu - {}\n", body.len()).as_bytes()), Ok(None));
    assert_eq!(
        d.feed(body.as_bytes()),
        Ok(Some(Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into()], flags: DmenuFlags::default() }))
    );
}

#[test]
fn outputs_verbs_round_trip() {
    assert_eq!(encode_request(&Request::Outputs), b"alpheus-launcher/1 outputs\n");
    assert_eq!(encode_request(&Request::OutputsKeep), b"alpheus-launcher/1 outputs-keep\n");
    assert_eq!(encode_request(&Request::OutputsRevert), b"alpheus-launcher/1 outputs-revert\n");
    assert_eq!(encode_request(&Request::OutputsTry("ab\n".into())), b"alpheus-launcher/1 outputs-try 3\nab\n");
    // The body travels in pieces as well, and its length is checked at once.
    let wire = encode_request(&Request::OutputsTry("step\tA\toff\n".into()));
    let mut d = RequestDecoder::new();
    for b in &wire[..wire.len() - 1] {
        assert_eq!(d.feed(&[*b]), Ok(None));
    }
    assert_eq!(d.feed(&wire[wire.len() - 1..]), Ok(Some(Request::OutputsTry("step\tA\toff\n".into()))));
    let too_big = format!("{MAGIC} outputs-try {}\n", crate::displays::wire::MAX_OUTPUTS_BODY + 1);
    assert_eq!(decode(too_big.as_bytes()), Err(IpcError::TooLarge("outputs body")));
    assert!(decode(format!("{MAGIC} outputs-try\n").as_bytes()).is_err());
    assert!(decode(format!("{MAGIC} outputs-try x\n").as_bytes()).is_err());
    assert!(decode(format!("{MAGIC} outputs-try 2\nabc").as_bytes()).is_err(), "bytes after the body");
    let mut not_utf8 = format!("{MAGIC} outputs-try 2\n").into_bytes();
    not_utf8.extend_from_slice(&[0xff, 0xfe]);
    assert!(decode(&not_utf8).is_err(), "not UTF-8");
    for bare in ["outputs", "outputs-keep", "outputs-revert"] {
        assert!(decode(format!("{MAGIC} {bare} x\n").as_bytes()).is_err(), "{bare} takes no word");
    }
    assert!(decode(format!("{MAGIC} outputs-try 0 x\n").as_bytes()).is_err());
}

#[test]
fn outputs_reply_carries_its_body() {
    let r = Reply::Outputs("status ready\ntrial none\n".into());
    assert_eq!(encode_reply(&r), b"outputs 24\nstatus ready\ntrial none\n");
    assert_eq!(decode_reply(&encode_reply(&r)), Ok(r));
    let empty = Reply::Outputs(String::new());
    assert_eq!(decode_reply(&encode_reply(&empty)), Ok(empty));
    assert_eq!(decode_reply(b"outputs 5\nab"), Err(IpcError::Truncated));
    assert!(matches!(decode_reply(b"outputs 1\nab"), Err(IpcError::Malformed(_))));
    assert!(matches!(decode_reply(b"outputs x\n"), Err(IpcError::Malformed(_))));
    assert!(matches!(decode_reply(b"outputs 1\n\xff"), Err(IpcError::Malformed(_))));
    let too_big = format!("outputs {}\n", crate::displays::wire::MAX_OUTPUTS_BODY + 1);
    assert_eq!(decode_reply(too_big.as_bytes()), Err(IpcError::TooLarge("outputs body")));
}

#[test]
fn an_unknown_trial_word_is_refused() {
    use crate::displays::wire::{Snapshot, Status, TrialEnd, TrialState, decode_snapshot, encode_snapshot};
    let snapshot = Snapshot { status: Status::Ready, outputs: vec![], trial: TrialState::Ended(TrialEnd::Kept) };
    let body = encode_snapshot(&snapshot);
    // The reply passes the body whole; the snapshot reads it strictly.
    let Ok(Reply::Outputs(back)) = decode_reply(&encode_reply(&Reply::Outputs(body.clone()))) else { panic!("not an outputs reply") };
    assert_eq!(decode_snapshot(&back), Ok(snapshot));
    let odd = body.replace("kept", "forgotten");
    let Ok(Reply::Outputs(back)) = decode_reply(&encode_reply(&Reply::Outputs(odd))) else { panic!("not an outputs reply") };
    assert!(decode_snapshot(&back).is_err());
}

#[test]
fn a_trial_waits_longer_than_the_other_verbs_and_dmenu_waits_for_the_person() {
    assert_eq!(Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }.reply_timeout(), None);
    assert_eq!(Request::OutputsTry(String::new()).reply_timeout(), Some(Duration::from_secs(7)));
    for r in
        [Request::Toggle, Request::Outputs, Request::OutputsKeep, Request::OutputsRevert, Request::Quick(QuickVerb::Top), Request::Calc]
    {
        assert_eq!(r.reply_timeout(), Some(Duration::from_secs(2)), "{r:?}");
    }
}

#[test]
fn exchange_tells_a_missing_socket_from_other_failures() {
    let path = std::env::temp_dir().join(format!("alpheus-ipc-{}-absent", std::process::id()));
    let e = exchange(&path, &Request::Toggle, REPLY_TIMEOUT).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
    assert!(e.to_string().contains("not running"), "{e}");
}

#[test]
fn timed_dnd_has_its_wire_and_old_words_stay() {
    assert_eq!(encode_request(&Request::DndOn(DndEnd::For(1800))), b"alpheus-launcher/1 dnd on for 1800\n");
    assert_eq!(encode_request(&Request::DndOn(DndEnd::Until(1_763_229_600))), b"alpheus-launcher/1 dnd on until 1763229600\n");
    assert_eq!(decode(b"alpheus-launcher/1 dnd on for 1800\n"), Ok(Some(Request::DndOn(DndEnd::For(1800)))));
    assert_eq!(decode(b"alpheus-launcher/1 dnd on until 1763229600\n"), Ok(Some(Request::DndOn(DndEnd::Until(1_763_229_600)))));
    assert_eq!(decode(b"alpheus-launcher/1 dnd on\n"), Ok(Some(Request::Dnd(DndVerb::On))));
    for h in [
        &b"alpheus-launcher/1 dnd off for 60\n"[..],
        b"alpheus-launcher/1 dnd on for x\n",
        b"alpheus-launcher/1 dnd on later 5\n",
        b"alpheus-launcher/1 dnd on for\n",
        b"alpheus-launcher/1 dnd on for 4294967296\n", // u32::MAX + 1: the conversion refuses, never wraps
    ] {
        assert!(matches!(decode(h), Err(IpcError::Malformed(_))), "{:?} -> {:?}", String::from_utf8_lossy(h), decode(h));
    }
    assert_eq!((DND_MIN_SECS, DND_MAX_SECS), (60, 86_400));
}
