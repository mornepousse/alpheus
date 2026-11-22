use super::*;

fn inputs() -> ReportInputs {
    ReportInputs {
        when: "2026-11-21 21:40 UTC".into(),
        alpheus: "0.1.0".into(),
        systemd: "260 (260.5)".into(),
        kernel: "6.18.2".into(),
        compositor: "Mango".into(),
        os: "NixOS 26.05 (Yarara)".into(),
        failed: Some(vec![ReportUnit { unit: "mnt-data.mount".into(), scope: "system", description: "/mnt/data".into() }]),
        loops: Some(vec![ReportLoop { unit: "example-sync.service".into(), scope: "user", restarts: 7 }]),
        crashes: Some(vec![ReportCrash { time: "Nov 20 14:02".into(), exe: "example-app".into(), signal: "SIGSEGV".into() }]),
        errors: Some(vec![
            ReportError {
                time: "21:02:11".into(),
                ident: "kernel".into(),
                message: "usb 3-2: device descriptor read/64, error -71".into(),
            },
            ReportError {
                time: "21:02:12".into(),
                ident: "kernel".into(),
                message: "usb 3-2: device descriptor read/64, error -71".into(),
            },
            ReportError { time: "21:03:00".into(), ident: "example-app".into(), message: "failed to open /mnt/data".into() },
        ]),
    }
}

fn none() -> Redactions {
    Redactions::default()
}

fn err(n: usize, len: usize) -> ReportError {
    ReportError { time: format!("21:{:02}:00", n % 60), ident: "kernel".into(), message: format!("{n:04} {}", "x".repeat(len)) }
}

#[test]
fn the_report_holds_only_the_listed_fields() {
    let text = build(&inputs(), &none());
    assert_eq!(
        text,
        "alpheus diagnostics report, 2026-11-21 21:40 UTC\n\
         alpheus 0.1.0 · systemd 260 (260.5) · kernel 6.18.2 · Mango · NixOS 26.05 (Yarara)\n\
         Failed units: mnt-data.mount (system: /mnt/data)\n\
         Restarting in a loop: example-sync.service (user, 7 restarts)\n\
         Crashes (7 days): Nov 20 14:02 example-app SIGSEGV\n\
         Errors this boot (latest 2, duplicates folded):\n \
         21:02:12 kernel: usb 3-2: device descriptor read/64, error -71 (×2)\n \
         21:03:00 example-app: failed to open /mnt/data\n"
    );
    let empty =
        ReportInputs { when: "t".into(), alpheus: "0.1.0".into(), failed: Some(vec![]), errors: Some(vec![]), ..Default::default() };
    let empty = build(&empty, &none());
    assert!(empty.contains("Failed units: none") && empty.contains("Errors this boot: none"), "{empty}");
    // A read that failed is not "none".
    let unknown = build(&ReportInputs { when: "t".into(), alpheus: "0.1.0".into(), ..Default::default() }, &none());
    assert!(
        unknown.contains("Failed units: unknown")
            && unknown.contains("Crashes (7 days): unknown")
            && unknown.contains("Errors this boot: unknown"),
        "{unknown}"
    );
}

#[test]
fn redaction_cases() {
    let r =
        Redactions::new(&["alice".into()], &["/home/alice".into()], Some("host-01.example.org"), Some("0123456789abcdef0123456789abcdef"));
    let cases = [
        ("opened /home/alice/notes by alice", "opened ~/notes by <user>"),
        ("on host-01.example.org now", "on <host> now"),
        ("id 0123456789abcdef0123456789abcdef", "id <machine-id>"),
        ("link 00:1a:2b:3c:4d:5e up", "link <mac> up"),
        ("link 00-1A-2B-3C-4D-5E up", "link <mac> up"),
        ("from 192.168.1.20: refused", "from <ip>: refused"),
        ("v6 fe80::1ff:fe23:4567:890a and 2001:db8:0:0:0:0:0:1 ok", "v6 <ip> and <ip> ok"),
        ("mail bob.smith+x@example.org, thanks", "mail <email>, thanks"),
        ("a@b is not one", "a@b is not one"),
        // Not addresses: a clock, a kernel version, an out-of-range quad, a version with five parts.
        ("21:02:11 kernel 6.18.2 999.1.1.1", "21:02:11 kernel 6.18.2 999.1.1.1"),
        ("device (wlan0): state 12:34", "device (wlan0): state 12:34"),
        // A port after the address stays; the address goes.
        ("dns 192.168.1.1:53 up", "dns <ip>:53 up"),
        // A word glued to the address by punctuation does not hide it.
        ("BSSID:aa:bb:cc:dd:ee:ff", "BSSID:<mac>"),
        ("wlan0-aa:bb:cc:dd:ee:ff", "wlan0-<mac>"),
        ("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF", "/org/bluez/hci0/dev_<mac>"),
        ("via ::ffff:10.1.2.3 ok", "via <ip> ok"),
        ("[2001:db8::1]:443", "[<ip>]:443"),
        // Not addresses: a Rust path, a version after the word, a letter-prefixed quad.
        ("std::io failed", "std::io failed"),
        ("a::b and Foo::Bar", "a::b and Foo::Bar"),
        ("version 1.2.3.4", "version 1.2.3.4"),
        ("v1.2.3.4 and 1.2.3.4.5", "v1.2.3.4 and 1.2.3.4.5"),
        ("peer 1.2.3.4 gone", "peer <ip> gone"),
        // Names match in any case, and the short name of a fully qualified host does too.
        ("Alice ALICE", "<user> <user>"),
        ("HOST-01.example.org and host-01 and host-01.other.net", "<host> and <host> and <host>.other.net"),
        // UUIDs, plain and in a unit name's escaping.
        ("fs 123e4567-e89b-12d3-a456-426614174000 ok", "fs <uuid> ok"),
        ("dev-disk-by\\x2duuid-123e4567\\x2de89b\\x2d12d3\\x2da456\\x2d426614174000.device", "dev-disk-by\\x2duuid-<uuid>.device"),
    ];
    for (input, want) in cases {
        assert_eq!(r.apply(input), want, "{input}");
    }
    // A two-letter name is not replaced everywhere: it would shred ordinary words.
    let short = Redactions::new(&["al".into()], &[], None, None);
    assert_eq!(short.apply("a real total"), "a real total");
    // The home is replaced before the user name, so the path keeps no name.
    assert_eq!(r.apply("/home/alice"), "~");
}

#[test]
fn errors_fold_and_keep_the_latest_40() {
    let mut i = inputs();
    let mut e: Vec<_> = (0..60).map(|n| err(n, 5)).collect();
    e.push(err(59, 5)); // one repeated
    i.errors = Some(e);
    let text = build(&i, &none());
    assert!(text.contains("(latest 40, duplicates folded)"), "{text}");
    assert!(text.contains("0059 xxxxx (×2)"), "{text}");
    assert!(text.contains("0020 xxxxx") && !text.contains("0019 xxxxx"), "the latest 40 of the 60 folded ones");
}

#[test]
fn cut_drops_oldest_errors_first() {
    let mut i = inputs();
    i.errors = Some((0..40).map(|n| err(n, 150)).collect());
    let text = build(&i, &none());
    assert!(text.len() <= 4096, "{}", text.len());
    for kept in ["alpheus diagnostics report", "Failed units: mnt-data.mount", "Restarting in a loop", "Crashes (7 days)"] {
        assert!(text.contains(kept), "{kept} missing:\n{text}");
    }
    assert!(text.contains("0039 "), "the newest error stays");
    assert!(!text.contains("0000 "), "the oldest goes first");
    let heading = text.lines().find(|l| l.starts_with("Errors this boot")).unwrap();
    let k: usize = heading.split("latest ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    assert!((12..=25).contains(&k), "about 15-20 fit: {heading}");
    assert!(heading.ends_with("of 40, older ones cut):"), "{heading}");
    assert_eq!(text.lines().filter(|l| l.starts_with(" 21:")).count(), k, "the heading counts what is shown");
}

#[test]
fn a_huge_fixed_part_is_cut_on_a_line() {
    let mut i = inputs();
    i.failed = Some(
        (0..200)
            .map(|n| ReportUnit { unit: format!("example-unit-{n:03}.service"), scope: "system", description: "d".repeat(50) })
            .collect(),
    );
    let text = build(&i, &none());
    assert!(text.len() <= 4096);
    assert!(text.ends_with("… cut\n"), "{text:?}");
    assert!(text.starts_with("alpheus diagnostics report"));
    assert!(text.lines().all(|l| l.len() < 4000), "cut on a line boundary");
}

#[test]
fn redaction_runs_on_the_final_text_and_the_size_is_of_the_redacted_text() {
    // A home path of 40 characters becomes `~`: more errors fit than the raw length allows.
    let home = format!("/home/{}", "a".repeat(34));
    let mut i = inputs();
    i.errors = Some(
        (0..40)
            .map(|n| ReportError { time: "21:00:00".into(), ident: "k".into(), message: format!("{n:04} {home}/{}", "x".repeat(100)) })
            .collect(),
    );
    let r = Redactions::new(&[], std::slice::from_ref(&home), None, None);
    let text = build(&i, &r);
    assert!(!text.contains(&home) && text.contains("~/"), "{text}");
    assert!(text.len() <= 4096);
    let kept = text.lines().filter(|l| l.starts_with(" 21:")).count();
    let raw = build(&i, &none()).lines().filter(|l| l.starts_with(" 21:")).count();
    assert!(kept > raw, "{kept} vs {raw}");
}

#[test]
fn kernel_release_is_not_the_node_name() {
    // SAFETY: a zeroed utsname is valid; the fields are filled below within their bounds.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    let put = |dst: &mut [libc::c_char], s: &str| {
        for (d, b) in dst.iter_mut().zip(s.bytes()) {
            *d = b as libc::c_char;
        }
    };
    put(&mut u.nodename, "my-secret-host");
    put(&mut u.release, "6.18.2");
    assert_eq!(release_of(&u), "6.18.2");
    assert!(!format!("{:?}", release_of(&u)).contains("secret"));
    assert_eq!(os_pretty("NAME=NixOS\nPRETTY_NAME=\"NixOS 26.05 (Yarara)\"\nID=nixos\n"), "NixOS 26.05 (Yarara)");
    assert_eq!(os_pretty("ID=x\n"), "");
}

#[test]
fn a_secret_is_redacted_before_the_line_is_cut() {
    // 190 characters, then a name that would straddle the 200-character cut.
    let mut i = inputs();
    i.errors =
        Some(vec![ReportError { time: "21:00:00".into(), ident: "k".into(), message: format!("{} alice-secret-name", "x".repeat(190)) }]);
    let r = Redactions::new(&["alice-secret-name".into()], &[], None, None);
    let text = build(&i, &r);
    assert!(!text.contains("alice") && !text.contains("secret-name"), "{text}");
    assert!(text.contains("<user>"), "redacted whole, then cut: {text}");
}

#[test]
fn a_user_named_like_a_placeholder_is_not_redacted_twice() {
    let mut i = inputs();
    i.errors = Some(vec![ReportError { time: "21:00:00".into(), ident: "k".into(), message: "hello user".into() }]);
    let r = Redactions::new(&["user".into()], &[], None, None);
    assert!(build(&i, &r).contains("k: hello <user>\n"), "{}", build(&i, &r));
}

#[test]
fn tag_characters_survive_only_after_a_flag() {
    let mut i = inputs();
    let flag = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}";
    i.errors = Some(vec![
        ReportError { time: "21:00:00".into(), ident: "k".into(), message: format!("flag {flag} ok") },
        ReportError { time: "21:00:01".into(), ident: "k".into(), message: "hidden \u{e0041}\u{e0042} tags".into() },
    ]);
    let text = build(&i, &none());
    assert!(text.contains(flag), "a subdivision flag is kept");
    assert!(!text.contains('\u{e0041}') && text.contains("hidden  tags"), "{text:?}");
}

#[test]
fn a_cut_that_fell_back_still_says_how_many_errors_there_were() {
    // Fixed parts of every length near the bound: the fallback never calls a read errors
    // "unknown" when they were read (for lengths just over the bound it used to).
    let mut base = inputs();
    base.errors = Some(vec![]);
    let mut units = Vec::new();
    while build(&ReportInputs { failed: Some(units.clone()), ..base.clone() }, &none()).len() < 3990 {
        let n = units.len();
        units.push(ReportUnit { unit: format!("example-unit-{n:03}.service"), scope: "system", description: "d".repeat(10) });
    }
    for pad in 0..120 {
        let mut i = inputs();
        let mut failed = units.clone();
        failed.push(ReportUnit { unit: "pad.service".into(), scope: "system", description: "p".repeat(pad.min(60)) });
        failed.push(ReportUnit { unit: "x".repeat(pad.saturating_sub(60) + 1), scope: "system", description: String::new() });
        i.failed = Some(failed);
        i.errors = Some((0..3).map(|n| err(n, 5)).collect());
        let text = build(&i, &none());
        assert!(text.len() <= 4096 && !text.contains("Errors this boot: unknown"), "{pad}: {text}");
    }
}
