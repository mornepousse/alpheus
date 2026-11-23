//! Tests of the notification server's model (`mod.rs`): the cleaners, ids, tags and merging, the
//! timeouts, the spam guards, the bubbles' queue, the history, do-not-disturb and the rules. No
//! clock, no bus and no daemon: each test passes the `Instant` it wants.

use super::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn center() -> Center {
    Center::new(&NotificationsConfig::default(), false)
}

fn note(app: &str, summary: &str) -> Incoming {
    Incoming {
        app_name: app.into(),
        replaces_id: 0,
        summary: summary.into(),
        body: String::new(),
        actions: Vec::new(),
        urgency: Urgency::Normal,
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: None,
    }
}

fn critical(app: &str, summary: &str) -> Incoming {
    Incoming { urgency: Urgency::Critical, ..note(app, summary) }
}

fn hidden(summary: &str) -> Incoming {
    Incoming {
        app_name: "Fictional".into(),
        replaces_id: 0,
        summary: summary.into(),
        body: String::new(),
        actions: Vec::new(),
        urgency: Urgency::Normal,
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: None,
    }
}

/// NameLost and a dead bus: every bubble goes, silently, the history stays.
#[test]
fn hide_all_drops_every_bubble_and_keeps_the_history() {
    let mut c = Center::new(&NotificationsConfig::default(), false);
    let now = Instant::now();
    for s in ["a", "b", "c", "d", "e"] {
        c.notify(hidden(s), now, 600); // five: one more than max_visible waits its turn
    }
    c.notify(Incoming { urgency: Urgency::Critical, ..hidden("sticky") }, now, 600);
    assert!(!c.bubbles().is_empty());
    c.hide_all();
    assert!(c.bubbles().is_empty(), "no bubble, not even a waiting one");
    assert_eq!(c.next_expiry(), None, "nothing left to time");
    assert_eq!(c.history().count(), 6, "the history stays for the panel");
    assert!(c.history().all(|e| !e.live));
    assert!(c.expire(now + Duration::from_secs(3600)).is_empty(), "nothing to expire, nothing to emit");
}

fn with_actions(n: Incoming, keys: &[&str]) -> Incoming {
    Incoming { actions: keys.iter().map(|k| Action { key: k.to_string(), label: k.to_uppercase() }).collect(), ..n }
}

/// The ids of the bubbles shown, top first.
fn shown(c: &Center) -> Vec<u32> {
    c.bubbles().iter().map(|s| s.entry.id).collect()
}

fn history(c: &Center) -> Vec<&str> {
    c.history().map(|e| e.summary.as_str()).collect()
}

fn live(c: &Center) -> usize {
    c.slots.iter().filter(|s| s.entry.live).count()
}

// ---- cleaning (spec § 2, Fields) ----

#[test]
fn markup_is_stripped_and_the_five_entities_decoded() {
    assert_eq!(strip_markup("<b>Download</b> finished"), "Download finished");
    assert_eq!(strip_markup("<a href=\"https://example.org/?a=1&amp;b=2\">link</a>"), "link", "an attribute with an entity");
    assert_eq!(strip_markup("Tom &amp; Jerry &lt;3 &gt; &quot;hi&quot; &apos;x&apos;"), "Tom & Jerry <3 > \"hi\" 'x'");
    assert_eq!(strip_markup("&amp;lt;b&amp;gt;"), "&lt;b&gt;", "decoded once, never twice");
    assert_eq!(strip_markup("&lt;b&gt;bold&lt;/b&gt;"), "<b>bold</b>", "an escaped tag is text, not markup");
    assert_eq!(strip_markup("&nbsp;&#39; & alone &amp"), "&nbsp;&#39; & alone &amp", "anything but the five stays as sent");
}

#[test]
fn hostile_markup_never_hides_text_nor_panics() {
    assert_eq!(strip_markup("<b><i><u>deep</u></i></b>"), "deep", "nested");
    assert_eq!(strip_markup("1 < 2 and 3 > 2"), "1 < 2 and 3 > 2", "a < before a space opens no tag");
    assert_eq!(strip_markup("a <b"), "a <b", "unclosed at the end: text");
    assert_eq!(strip_markup("x <oops and <b>more</b>"), "x <oops and more", "a < inside a tag: the first one was text");
    assert_eq!(strip_markup("<script>alert('x')</script>"), "alert('x')", "a script is text, never run");
    assert_eq!(strip_markup("<a title=\"1 > 0\">ok</a>"), "ok", "a > inside quotes ends no tag");
    assert_eq!(strip_markup("<!-- note --><?xml?>text"), "text");
    assert_eq!(strip_markup("<é>x"), "<é>x", "< then a non-ASCII letter opens no tag");
    assert_eq!(strip_markup("<x \"unclosed quote> then <b>b</b>"), "<x \"unclosed quote> then <b>b</b>", "unclosed to the end: all text");
    for s in ["", "<", ">", "&", "<<<", "</", "<!"] {
        assert_eq!(strip_markup(s), s, "{s:?}");
    }
    for hostile in ["<".repeat(100_000), "<a".repeat(50_000), "<a \"".repeat(25_000), "&amp;".repeat(20_000), "<b>".repeat(33_000)] {
        let body = body_text(&hostile);
        assert!(body.len() <= MAX_BODY, "{} bytes from {}", body.len(), &hostile[..8]);
    }
}

/// A body of only markup strips to empty; the notification still shows.
#[test]
fn a_body_of_only_markup_strips_to_empty_and_still_shows() {
    assert_eq!(body_text("<b></b><i><u></u></i><br/>"), "", "a line break alone is trimmed away");
    assert_eq!(body_text("<b></b>\n<i> </i>"), "", "and the blank lines left behind are trimmed");
    let (mut c, t0) = (center(), Instant::now());
    let id = c.notify(Incoming { body: body_text("<b></b>"), ..note("Firefox", "Download finished") }, t0, 0);
    assert_eq!(shown(&c), [id], "a summary alone is a notification");
    assert_eq!(c.entry(id).map(|e| e.body.as_str()), Some(""));
}

#[test]
fn a_100_kb_field_keeps_its_cap_cut_on_a_character() {
    let huge = "é".repeat(50_000) + "tail";
    let body = body_text(&huge);
    assert_eq!(body.len(), MAX_BODY);
    assert!(body.chars().all(|c| c == 'é'));
    assert_eq!(summary_text(&"日本".repeat(20_000)).len(), 255, "85 three-byte characters: the 86th would pass 256");
    assert_eq!(app_text(&"a".repeat(100_000)).len(), MAX_APP);
    assert_eq!(hint_text(&"x".repeat(100_000)).map(|h| h.len()), Some(MAX_HINT));
}

#[test]
fn cap_bytes_never_splits_a_character() {
    assert_eq!(cap_bytes("abc", 3), "abc");
    assert_eq!(cap_bytes("abcd", 3), "abc");
    assert_eq!(cap_bytes("aé", 2), "a", "é is two bytes");
    assert_eq!(cap_bytes("日本", 5), "日");
    assert_eq!(cap_bytes("🦀x", 3), "", "four bytes: nothing fits");
    assert_eq!(cap_bytes("🦀x", 4), "🦀");
    assert_eq!(cap_bytes("é", 0), "");
    let s = "aé日🦀b";
    for max in 0..12 {
        let cut = cap_bytes(s, max);
        assert!(cut.len() <= max && s.starts_with(&cut), "{max}: {cut:?}");
    }
}

#[test]
fn controls_and_bidi_overrides_go_and_newlines_stay_in_the_body_only() {
    assert_eq!(body_text("line 1\r\nline 2\n\tline 3"), "line 1\nline 2\n line 3");
    assert_eq!(summary_text("two\nlines"), "two lines");
    assert_eq!(summary_text("evil\u{202e}gnp.exe"), "evilgnp.exe", "a right-to-left override removed");
    assert_eq!(body_text("a\u{1b}[31mb\u{0}c\u{200b}d"), "a[31mbcd");
    assert_eq!(summary_text("a\u{200d}b"), "a\u{200d}b", "a ZWJ is text shaping: kept");
    assert_eq!(app_text("  Firefox \n"), "Firefox", "trimmed");
    assert_eq!(app_text("<b>Firefox</b>"), "<b>Firefox</b>", "the app name is never parsed as markup");
    assert_eq!(body_text("\n\n  text  \n\n"), "text");
    assert_eq!(hint_text(" \t\n"), None, "blank: absent");
    assert_eq!(hint_text(" firefox\n"), Some("firefox".into()));
}

#[test]
fn line_breaks_in_markup_and_unicode_separators_are_line_breaks() {
    assert_eq!(body_text("one<br>two<br/>three<br />four<BR>five"), "one\ntwo\nthree\nfour\nfive");
    assert_eq!(summary_text("one<br>two<br/>three"), "one two three", "one line: a space");
    assert_eq!(body_text("a\u{2028}b\u{2029}c"), "a\nb\nc");
    assert_eq!(summary_text("a\u{2028}b\u{2029}c"), "a b c");
    assert_eq!(body_text("<brick>wall</brick>"), "wall", "only br itself");
    assert_eq!(strip_markup("x<br>y"), "x\ny");
}

/// A `<br>` is known by its first token, cut at whitespace or `/`: spaces before the slash or
/// the `>`, and attributes, keep it a line break; a longer name never is one.
#[test]
fn a_br_with_spaces_or_attributes_is_a_line_break() {
    assert_eq!(body_text("a<br / >b<br/ >c<br clear=\"all\">d<BR\tclear=x>e<br\n/>f"), "a\nb\nc\nd\ne\nf");
    assert_eq!(body_text("a<brx>b<br-x/>c<b r>d"), "abcd", "another name: removed, no break");
}

#[test]
fn an_action_needs_a_sane_key_and_its_label_is_capped() {
    assert_eq!(action("default", "Open"), Some(Action { key: "default".into(), label: "Open".into() }));
    assert_eq!(action("", "Empty key"), None);
    assert_eq!(action(&"k".repeat(MAX_KEY + 1), "Long"), None, "a key goes back as sent: refused, never cut");
    assert!(action(&"k".repeat(MAX_KEY), "Max").is_some());
    assert_eq!(action("a\u{1}b", "Control"), None);
    assert_eq!(action("reply", &"é".repeat(100)).map(|a| a.label.len()), Some(MAX_LABEL));
    assert_eq!(action("x", "<b>Reply</b>\n").map(|a| a.label), Some("<b>Reply</b>".into()), "a label is plain text");
}

// ---- timeouts (spec § 2, Timeout; docs/decisions.md, "A bubble's timeout follows its urgency") ----

#[test]
fn a_bubbles_timeout_follows_its_urgency_and_the_apps_own_choice() {
    use Urgency::{Critical, Low, Normal};
    let t = Timeouts::of(&NotificationsConfig::default());
    assert_eq!(timeout(Normal, -1, &t), Some(secs(5)), "-1: the default");
    assert_eq!(timeout(Low, -7, &t), Some(secs(5)), "any negative: the default");
    assert_eq!(timeout(Normal, 3_000, &t), Some(secs(3)));
    assert_eq!(timeout(Normal, 2_500, &t), Some(Duration::from_millis(2_500)));
    assert_eq!(timeout(Normal, 500, &t), Some(secs(2)), "clamped up to timeout_min");
    assert_eq!(timeout(Normal, 25_000, &t), Some(secs(20)), "Chromium's 25 s, down to timeout_max");
    assert_eq!(timeout(Normal, i32::MAX, &t), Some(secs(20)));
    assert_eq!(timeout(Normal, 0, &t), Some(secs(20)), "0: timeout_max unless critical");
    for ms in [-1, 0, 3_000] {
        assert_eq!(timeout(Critical, ms, &t), None, "critical is always sticky ({ms})");
    }
    let odd = Timeouts { default: secs(5), min: secs(30), max: secs(10) };
    assert_eq!(timeout(Normal, 1_000, &odd), Some(secs(10)), "min above max: no panic");
}

// ---- ids, revisions, tags (spec § 1, Ids) ----

#[test]
fn ids_count_from_1_and_an_unknown_replaces_id_makes_the_counter_jump() {
    let (mut c, t0) = (center(), Instant::now());
    assert_eq!(c.notify(note("a", "one"), t0, 0), 1);
    assert_eq!(c.notify(note("a", "two"), t0, 0), 2);
    assert_eq!(c.notify(Incoming { replaces_id: 7_000_000, ..note("Equibop", "tagged") }, t0, 0), 7_000_000, "taken as is");
    assert_eq!(c.notify(note("a", "three"), t0, 0), 7_000_001, "the counter jumps past it");
    assert_eq!(c.notify(Incoming { replaces_id: 5, ..note("b", "low") }, t0, 0), 5, "an unknown id below the counter");
    assert_eq!(c.notify(note("a", "four"), t0, 0), 7_000_002, "it never moves the counter back");
}

/// The counter wraps past u32::MAX to 1, never 0, never an id in use.
#[test]
fn ids_wrap_past_u32_max_to_1_and_skip_ids_still_in_use() {
    let (mut c, t0) = (center(), Instant::now());
    assert_eq!(c.notify(note("a", "first"), t0, 0), 1, "1 stays in use");
    assert_eq!(c.notify(Incoming { replaces_id: u32::MAX - 1, ..note("a", "near") }, t0, 0), u32::MAX - 1);
    assert_eq!(c.notify(note("a", "max"), t0, 0), u32::MAX);
    assert_eq!(c.notify(note("a", "wrapped"), t0, 0), 2, "past u32::MAX: 1 is taken, so 2; never 0");
    assert!(c.history().all(|e| e.id != 0));
}

#[test]
fn a_known_replaces_id_updates_in_place_and_shows_again() {
    let (mut c, t0) = (center(), Instant::now());
    let id = c.notify(note("Firefox", "Downloading 10%"), t0, 600);
    c.notify(note("Thunderbird", "Mail"), t0, 601);
    c.expire(t0 + secs(6));
    c.mark_read();
    assert!(c.bubbles().is_empty());
    let again = c.notify(Incoming { replaces_id: id, ..note("Firefox", "Downloading 90%") }, t0 + secs(7), 607);
    assert_eq!(again, id, "the same id");
    let e = c.entry(id).unwrap();
    assert_eq!((e.revision, e.summary.as_str(), e.live, e.read, e.at), (2, "Downloading 90%", true, false, 607));
    assert_eq!(history(&c), ["Downloading 90%", "Mail"], "one entry, now the newest");
    assert_eq!(shown(&c), [id], "shown again");
}

#[test]
fn a_late_click_on_a_replaced_bubble_does_nothing() {
    let (mut c, t0) = (center(), Instant::now());
    let id = c.notify(with_actions(note("Equibop", "Lina: hi"), &["default"]), t0, 0);
    c.notify(Incoming { replaces_id: id, ..with_actions(note("Equibop", "Lina: hi again"), &["default"]) }, t0, 0);
    assert!(c.invoke(id, 1, "default", t0).is_empty(), "revision 1 was replaced: stale");
    assert!(c.invoke(id, 2, "nope", t0).is_empty(), "a key it never offered");
    assert_eq!(c.invoke(id, 2, "default", t0), [Effect::Invoked(id, "default".into()), Effect::Closed(id, CloseReason::Dismissed)]);
}

#[test]
fn the_same_tag_from_the_same_app_replaces_it() {
    let (mut c, t0) = (center(), Instant::now());
    let tagged = |app: &str, summary: &str| Incoming { tag: Some("volume".into()), ..note(app, summary) };
    let id = c.notify(tagged("notify-send", "Volume 40%"), t0, 0);
    assert_eq!(c.notify(tagged("notify-send", "Volume 45%"), t0, 0), id, "same tag: same id");
    assert_eq!(history(&c), ["Volume 45%"]);
    assert_eq!(c.entry(id).map(|e| e.revision), Some(2));
    assert_ne!(c.notify(tagged("other-script", "Volume 50%"), t0, 0), id, "another application's tag is its own");
    assert_eq!(c.history().count(), 2);
}

// ---- spam (spec § 2, Spam) ----

#[test]
fn an_identical_notification_within_one_second_merges() {
    let (mut c, t0) = (center(), Instant::now());
    let ms = Duration::from_millis;
    let id = c.notify(note("Slack", "ping"), t0, 0);
    assert_eq!(c.notify(note("Slack", "ping"), t0 + ms(500), 0), id, "within 1 s: the same one");
    assert_eq!(c.notify(note("Slack", "ping"), t0 + ms(1_400), 0), id, "the window slides with each merge");
    assert_eq!(c.history().count(), 1);
    assert_ne!(c.notify(note("Slack", "ping"), t0 + ms(2_500), 0), id, "1.1 s of quiet: a new one");
    let pong = c.notify(note("Slack", "pong"), t0 + ms(2_600), 0);
    assert_ne!(c.notify(Incoming { body: "more".into(), ..note("Slack", "pong") }, t0 + ms(2_700), 0), pong, "another body");
}

#[test]
fn a_merge_never_swallows_a_higher_urgency_nor_a_bubble_owed() {
    let (mut c, t0) = (center(), Instant::now());
    let ms = Duration::from_millis;
    let normal = c.notify(note("upower", "Battery low"), t0, 0);
    let urgent = c.notify(critical("upower", "Battery low"), t0 + ms(100), 0);
    assert_ne!(urgent, normal, "normal then critical: a new one");
    assert_eq!(c.entry(urgent).map(|e| e.urgency), Some(Urgency::Critical));
    assert_eq!(shown(&c), [urgent, normal]);

    let mut c = Center::new(&NotificationsConfig::default(), true);
    let quiet = c.notify(note("Slack", "ping"), t0, 0);
    assert_eq!(c.notify(note("Slack", "ping"), t0 + ms(100), 0), quiet, "both history only: merged");
    c.set_dnd(false);
    let loud = c.notify(note("Slack", "ping"), t0 + ms(200), 0);
    assert_ne!(loud, quiet, "the first had no bubble, this one gets one: never swallowed");
    assert_eq!(shown(&c), [loud]);
    assert_eq!(c.notify(note("Slack", "ping"), t0 + ms(300), 0), loud, "into a live one: merged");
    assert_eq!(c.history().count(), 2);

    let mut c = center();
    for i in 0..BUCKET {
        c.notify(note("spammer", &format!("n{i}")), t0, 0);
    }
    let refused = c.notify(note("spammer", "again"), t0, 0);
    assert!(!c.entry(refused).unwrap().live, "the bucket is empty");
    assert_eq!(c.notify(note("spammer", "again"), t0 + ms(100), 0), refused, "still no token: merged, no duplicate");

    let mut c = center();
    for i in 0..MAX_BUCKETS {
        let id = c.notify(note(&format!("app{i}"), "x"), t0, 0);
        c.close(id, CloseReason::Dismissed, t0);
    }
    let newcomer = c.notify(note("newcomer", "hi"), t0, 0);
    assert!(!c.entry(newcomer).unwrap().live, "no bucket left for a new identity");
    assert_eq!(c.notify(note("newcomer", "hi"), t0 + ms(100), 0), newcomer, "still none: merged");
}

/// A merge slides the window but never outlives the bucket: the token that comes back a
/// second after the bucket ran dry gives the next copy its bubble, even inside the window.
#[test]
fn a_token_back_ends_a_merge_inside_a_sliding_window() {
    let (mut c, t0) = (center(), Instant::now());
    let ms = Duration::from_millis;
    for i in 0..BUCKET {
        c.notify(note("spammer", &format!("n{i}")), t0, 0);
    }
    let dry = c.notify(note("spammer", "again"), t0, 0);
    assert!(!c.entry(dry).unwrap().live, "the bucket is dry at t0");
    assert_eq!(c.notify(note("spammer", "again"), t0 + ms(900), 0), dry, "+0.9 s, no token yet: merged, the window slides");
    let fresh = c.notify(note("spammer", "again"), t0 + ms(1_100), 0);
    assert_ne!(fresh, dry, "+1.1 s, still in the slid window, but a token is back: a new one");
    assert!(c.entry(fresh).unwrap().live, "with its bubble");
}

#[test]
fn ten_bubbles_then_one_per_second_and_critical_bypasses_the_bucket() {
    let (mut c, t0) = (center(), Instant::now());
    for i in 0..10 {
        c.notify(note("spammer", &format!("n{i}")), t0, 0);
    }
    assert_eq!(live(&c), 10);
    let id = c.notify(note("spammer", "n10"), t0, 0);
    assert!(!c.entry(id).unwrap().live, "the 11th: history only");
    assert_eq!(c.history().next().map(|e| e.id), Some(id), "but in the history");
    let urgent = c.notify(critical("spammer", "disk full"), t0, 0);
    assert!(c.entry(urgent).unwrap().live, "critical bypasses the bucket");
    let other = c.notify(note("Firefox", "done"), t0, 0);
    assert!(c.entry(other).unwrap().live, "one bucket per application");
    let id = c.notify(note("spammer", "n11"), t0 + Duration::from_millis(1_000), 0);
    assert!(c.entry(id).unwrap().live, "one token back after a second");
    let id = c.notify(note("spammer", "n12"), t0 + Duration::from_millis(1_500), 0);
    assert!(!c.entry(id).unwrap().live, "and only one");
}

/// An application sending 1000 notifications in a loop.
#[test]
fn an_app_sending_1000_notifications_in_a_loop_stays_bounded() {
    let (mut c, t0) = (center(), Instant::now());
    let ids: Vec<u32> = (0..1000).map(|i| c.notify(note("spammer", &format!("n{i}")), t0, 0)).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "every one answered with its own id");
    assert_eq!(c.history().count(), 50, "the history cap");
    assert_eq!(c.history().next().map(|e| e.summary.as_str()), Some("n999"));
    assert_eq!(live(&c), 10, "the bucket: 10 bubbles");
    assert_eq!(shown(&c), [ids[9], ids[8], ids[7], ids[6]], "4 shown, newest first");
    assert!(c.slots.len() <= 50 + MAX_LIVE, "{} entries", c.slots.len());
    assert_eq!(c.buckets.len(), 1);
    assert_eq!(c.view(true).unread, 50);
    let same: Vec<u32> = (0..1000).map(|_| c.notify(note("looper", "same"), t0, 0)).collect();
    assert!(same.iter().all(|id| *id == same[0]), "identical ones merge into one");
    let mut c = center();
    for i in 0..1000 {
        c.notify(note(&format!("app{i}"), "x"), t0, 0);
    }
    assert_eq!(live(&c), MAX_LIVE, "a new name each time: still at most 32 bubbles");
    assert!(c.buckets.len() <= MAX_BUCKETS);
    assert!(c.slots.len() <= 50 + MAX_LIVE);
}

// ---- history, transient, closing (spec § 2, § 3, § 4) ----

#[test]
fn the_history_keeps_the_newest_and_0_keeps_none() {
    let t0 = Instant::now();
    let mut c = Center::new(&NotificationsConfig { history: 3, ..NotificationsConfig::default() }, false);
    for s in ["a", "b", "c", "d"] {
        c.notify(note("x", s), t0, 0);
    }
    assert_eq!(history(&c), ["d", "c", "b"]);
    assert_eq!(shown(&c).len(), 4, "a left the history; its bubble stays");
    c.expire(t0 + secs(5));
    assert_eq!(c.slots.len(), 3, "once expired, the evicted one is gone");
    let mut none = Center::new(&NotificationsConfig { history: 0, ..NotificationsConfig::default() }, false);
    let id = none.notify(note("x", "only a bubble"), t0, 0);
    assert_eq!(none.history().count(), 0);
    assert_eq!(shown(&none), [id], "bubbles still show");
    assert_eq!(none.view(true).unread, 0);
    none.expire(t0 + secs(5));
    assert!(none.slots.is_empty(), "nothing kept");
}

#[test]
fn a_transient_notification_is_a_bubble_only() {
    let (mut c, t0) = (center(), Instant::now());
    let id = c.notify(Incoming { transient: true, ..note("blueman", "Connected") }, t0, 0);
    assert_eq!(shown(&c), [id]);
    assert_eq!((c.history().count(), c.view(true).unread), (0, 0));
    assert_eq!(c.expire(t0 + secs(5)), [Effect::Closed(id, CloseReason::Expired)]);
    assert!(c.entry(id).is_none(), "gone");
}

#[test]
fn closing_by_expiry_dismissal_or_call() {
    let (mut c, t0) = (center(), Instant::now());
    let expired = c.notify(note("a", "expires"), t0, 0);
    let dismissed = c.notify(note("a", "dismissed"), t0, 0);
    let called = c.notify(note("a", "closed by its app"), t0, 0);
    assert_eq!(c.close(dismissed, CloseReason::Dismissed, t0), [Effect::Closed(dismissed, CloseReason::Dismissed)]);
    assert_eq!(c.entry(dismissed).map(|e| (e.live, e.read)), Some((false, true)), "kept, read");
    assert_eq!(c.close(called, CloseReason::Closed, t0), [Effect::Closed(called, CloseReason::Closed)]);
    assert!(c.entry(called).is_none(), "the app's CloseNotification removes it");
    assert_eq!(c.expire(t0 + secs(5)), [Effect::Closed(expired, CloseReason::Expired)]);
    assert_eq!(c.entry(expired).map(|e| (e.live, e.read)), Some((false, false)), "expired: in the history, unread");
    assert!(c.close(expired, CloseReason::Closed, t0).is_empty(), "not live: no signal");
    assert!(c.entry(expired).is_none(), "but removed");
    assert!(c.close(424_242, CloseReason::Closed, t0).is_empty(), "unknown: nothing");
    assert_eq!(c.history().count(), 1);
}

#[test]
fn an_action_closes_its_notification_unless_resident() {
    let (mut c, t0) = (center(), Instant::now());
    let id = c.notify(with_actions(note("Firefox", "Download finished"), &["default", "open"]), t0, 0);
    assert_eq!(c.invoke(id, 1, "open", t0), [Effect::Invoked(id, "open".into()), Effect::Closed(id, CloseReason::Dismissed)]);
    assert!(c.entry(id).is_none(), "done: out of the history too");
    let r = c.notify(Incoming { resident: true, ..with_actions(note("Rhythmbox", "Now playing"), &["next"]) }, t0, 0);
    assert_eq!(c.invoke(r, 1, "next", t0), [Effect::Invoked(r, "next".into())]);
    assert_eq!(c.entry(r).map(|e| (e.live, e.read)), Some((true, true)), "resident: stays");
    c.expire(t0 + secs(5));
    assert!(c.invoke(r, 1, "next", t0).is_empty(), "no longer live: nothing runs");
}

#[test]
fn the_next_expiry_is_the_earliest_and_none_at_rest() {
    let (mut c, t0) = (center(), Instant::now());
    assert_eq!(c.next_expiry(), None, "nothing shown: no timer");
    c.notify(Incoming { expire_ms: 10_000, ..note("a", "ten") }, t0, 0);
    c.notify(Incoming { expire_ms: 3_000, ..note("b", "three") }, t0 + secs(1), 0);
    c.notify(critical("c", "sticky"), t0, 0);
    assert_eq!(c.next_expiry(), Some(t0 + secs(4)));
    assert_eq!(c.expire(t0 + secs(4)).len(), 1);
    assert_eq!(c.next_expiry(), Some(t0 + secs(10)));
    c.expire(t0 + secs(10));
    assert_eq!(c.next_expiry(), None, "a critical bubble is sticky: no timer for it");
    assert_eq!(c.bubbles().len(), 1);
}

// ---- bubbles, do-not-disturb, unread (spec § 3, § 4) ----

#[test]
fn at_most_max_visible_bubbles_newest_first_extras_wait() {
    let (mut c, t0) = (center(), Instant::now());
    let ids: Vec<u32> = ["a", "b", "c", "d", "e"].iter().map(|s| c.notify(note(s, s), t0, 0)).collect();
    assert_eq!(shown(&c), [ids[4], ids[3], ids[2], ids[1]]);
    c.close(ids[3], CloseReason::Dismissed, t0);
    assert_eq!(shown(&c), [ids[4], ids[2], ids[1], ids[0]], "the waiting one shows as room frees");
    let urgent = c.notify(critical("f", "f"), t0, 0);
    let top = c.bubbles()[0];
    assert_eq!((top.entry.id, top.critical), (urgent, true));
    assert!(!c.bubbles()[1].critical);
}

/// Spec § 3, "extras wait and show as space frees up": a bubble waiting beyond `max_visible`
/// has no expiry yet; it gets its whole timeout from the moment it shows.
#[test]
fn a_waiting_bubble_gets_its_full_timeout_once_shown() {
    let (mut c, t0) = (center(), Instant::now());
    let ids: Vec<u32> = ["a", "b", "c", "d", "e", "f"].iter().map(|s| c.notify(note(s, s), t0, 0)).collect();
    assert_eq!(shown(&c), [ids[5], ids[4], ids[3], ids[2]], "four show, two wait");
    assert_eq!(c.next_expiry(), Some(t0 + secs(5)));
    let first: Vec<Effect> = ids[2..].iter().rev().map(|&id| Effect::Closed(id, CloseReason::Expired)).collect();
    let mut gone = c.expire(t0 + secs(5));
    gone.sort_by_key(|e| match e {
        Effect::Closed(id, _) | Effect::Invoked(id, _) => std::cmp::Reverse(*id),
    });
    assert_eq!(gone, first, "only the four shown expire");
    assert_eq!(shown(&c), [ids[1], ids[0]], "the two waiting show now");
    assert_eq!(c.next_expiry(), Some(t0 + secs(10)), "with their whole timeout from now");
    assert!(c.expire(t0 + secs(9)).is_empty());
    assert_eq!(c.expire(t0 + secs(10)).len(), 2);
    assert_eq!(c.next_expiry(), None);
}

/// A dismissal frees room too: the bubble that takes it starts its timeout then.
#[test]
fn a_bubble_shown_by_a_dismissal_starts_its_timeout_then() {
    let (mut c, t0) = (center(), Instant::now());
    let ids: Vec<u32> = ["a", "b", "c", "d", "e"].iter().map(|s| c.notify(note(s, s), t0, 0)).collect();
    c.close(ids[4], CloseReason::Dismissed, t0 + secs(3));
    assert_eq!(shown(&c), [ids[3], ids[2], ids[1], ids[0]]);
    assert_eq!(c.expire(t0 + secs(5)).len(), 3, "the three shown at t0");
    assert_eq!(c.next_expiry(), Some(t0 + secs(8)), "the one shown at +3 s");
    // An action run frees room the same way.
    let (mut c, t0) = (center(), Instant::now());
    let ids: Vec<u32> = ["a", "b", "c", "d", "e"].iter().map(|s| c.notify(with_actions(note(s, s), &["go"]), t0, 0)).collect();
    assert_eq!(c.invoke(ids[4], 1, "go", t0 + secs(2)).len(), 2);
    assert_eq!(c.expire(t0 + secs(5)).len(), 3);
    assert_eq!(c.next_expiry(), Some(t0 + secs(7)), "shown at +2 s");
}

/// Critical bubbles come first, newest first; then the others, newest first.
#[test]
fn critical_bubbles_show_before_the_others() {
    let (mut c, t0) = (center(), Instant::now());
    let a = c.notify(note("a", "a"), t0, 0);
    let x = c.notify(critical("x", "x"), t0, 0);
    let b = c.notify(note("b", "b"), t0, 0);
    let y = c.notify(critical("y", "y"), t0, 0);
    let d = c.notify(note("d", "d"), t0, 0);
    assert_eq!(shown(&c), [y, x, d, b], "a waits behind the two criticals");
    assert!(c.entry(a).unwrap().live);
}

/// A flood of distinct criticals never blocks a real one: past `max_visible` × 2 live criticals,
/// the oldest goes to the history, closed as expired.
#[test]
fn a_flood_of_criticals_never_blocks_a_real_one() {
    let (mut c, t0) = (center(), Instant::now());
    let flood: Vec<u32> = (0..8).map(|i| c.notify(critical("flooder", &format!("f{i}")), t0, 0)).collect();
    assert!(c.take_effects().is_empty(), "eight live criticals: the cap, nothing demoted");
    let real = c.notify(critical("upower", "Battery at 3%"), t0, 0);
    assert_eq!(c.take_effects(), [Effect::Closed(flood[0], CloseReason::Expired)], "the oldest demoted");
    assert!(c.take_effects().is_empty(), "taken once");
    assert_eq!(c.entry(flood[0]).map(|e| e.live), Some(false), "kept in the history");
    assert_eq!(shown(&c)[0], real, "the real one on top");
    for i in 8..40 {
        c.notify(critical("flooder", &format!("f{i}")), t0, 0);
    }
    assert_eq!(c.take_effects().len(), 32, "one demoted per newcomer");
    let crit = c.slots.iter().filter(|s| s.entry.live && s.entry.urgency == Urgency::Critical).count();
    assert_eq!(crit, 8, "never more than max_visible × 2");
    let last = c.notify(critical("udiskie", "Disk failing"), t0, 0);
    assert_eq!(shown(&c)[0], last, "a real one still shows after a long flood");
}

/// A copy merged into a live bubble still waiting beyond `max_visible` brings it to the top,
/// as a fresh one would, with the copy's time (the history stays newest first, times in order);
/// one already shown keeps its place and its time.
#[test]
fn a_merge_into_a_waiting_bubble_brings_it_to_the_top() {
    let (mut c, t0) = (center(), Instant::now());
    let ms = Duration::from_millis;
    let ids: Vec<u32> = ["a", "b", "c", "d", "e"].iter().map(|s| c.notify(note(s, s), t0, 600)).collect();
    assert_eq!(shown(&c), [ids[4], ids[3], ids[2], ids[1]], "a waits");
    assert_eq!(c.notify(note("a", "a"), t0 + ms(500), 601), ids[0], "merged");
    assert_eq!(shown(&c), [ids[0], ids[4], ids[3], ids[2]], "and on top");
    assert_eq!(history(&c), ["a", "e", "d", "c", "b"], "the newest in the history too");
    assert_eq!(c.entry(ids[0]).map(|e| e.at), Some(601), "with the copy's time");
    assert_eq!(c.notify(note("d", "d"), t0 + ms(600), 601), ids[3]);
    assert_eq!(shown(&c), [ids[0], ids[4], ids[3], ids[2]], "shown already: it stays where it is");
    assert_eq!(c.entry(ids[3]).map(|e| e.at), Some(600), "and keeps its time");
}

/// A copy merged into a waiting bubble shows it at once: its timeout starts then, and the one it
/// pushes back waits again, its timer stopped.
#[test]
fn a_merge_that_shows_a_waiting_bubble_starts_its_timeout() {
    let (mut c, t0) = (center(), Instant::now());
    let ms = Duration::from_millis;
    let ids: Vec<u32> = ["a", "b", "c", "d", "e"].iter().map(|s| c.notify(note(s, s), t0, 600)).collect();
    c.notify(note("a", "a"), t0 + ms(500), 600);
    assert_eq!(shown(&c), [ids[0], ids[4], ids[3], ids[2]]);
    let gone = c.expire(t0 + secs(5));
    assert_eq!(gone.len(), 3, "e, d and c; b was pushed back: {gone:?}");
    assert_eq!(shown(&c), [ids[0], ids[1]]);
    assert_eq!(c.next_expiry(), Some(t0 + ms(5_500)), "a, from the merge");
    assert_eq!(c.expire(t0 + ms(5_500)), [Effect::Closed(ids[0], CloseReason::Expired)]);
    assert_eq!(c.next_expiry(), Some(t0 + secs(10)), "b, shown again at +5 s, gets a whole timeout");
}

/// A bubble that has shown keeps its expiry when newer ones push it back: an app sending at
/// about max_visible / timeout keeps the bubbles expiring, live stays small, and a critical one
/// still shows at once afterwards (otherwise nothing expired, live reached
/// MAX_LIVE in about 32 s and the critical got no bubble).
#[test]
fn a_steady_flow_keeps_expiring_and_a_critical_still_shows() {
    let (mut c, t0) = (center(), Instant::now());
    let mut expired = 0;
    for i in 0..40 {
        let at = t0 + secs(i);
        expired += c.expire(at).len();
        c.notify(note("chat", &format!("m{i}")), at, 0);
        c.seen();
        assert!(live(&c) <= 6, "at {i} s: {} live", live(&c));
    }
    assert!(expired >= 30, "the bubbles keep expiring during the flow: {expired}");
    let at = t0 + secs(40);
    let urgent = c.notify(critical("upower", "Battery at 3%"), at, 0);
    assert_eq!(shown(&c).first(), Some(&urgent), "the critical one shows at once");
}

/// Shown, then pushed back behind a newer one: it still expires when its timeout ends.
#[test]
fn a_bubble_pushed_back_after_it_showed_still_expires_on_time() {
    let (mut c, t0) = (center(), Instant::now());
    let ids: Vec<u32> = ["a", "b", "c", "d"].iter().map(|s| c.notify(note(s, s), t0, 0)).collect();
    c.seen();
    let e = c.notify(note("e", "e"), t0 + secs(2), 0);
    c.seen();
    assert_eq!(shown(&c), [e, ids[3], ids[2], ids[1]], "a pushed back");
    assert_eq!(c.next_expiry(), Some(t0 + secs(5)), "a's timer still runs");
    let gone = c.expire(t0 + secs(5));
    assert_eq!(gone.len(), 4, "a, b, c and d: {gone:?}");
    assert!(gone.contains(&Effect::Closed(ids[0], CloseReason::Expired)));
    assert_eq!(shown(&c), [e]);
    assert_eq!(c.next_expiry(), Some(t0 + secs(7)));
}

/// The live cap never blocks a critical one: the oldest live normal bubble goes to the
/// history, closed as expired, to make room.
#[test]
fn a_critical_one_gets_a_bubble_at_the_live_cap() {
    let (mut c, t0) = (center(), Instant::now());
    let ids: Vec<u32> = (0..MAX_LIVE).map(|i| c.notify(note(&format!("app{i}"), "x"), t0, 0)).collect();
    assert_eq!(live(&c), MAX_LIVE);
    assert!(c.take_effects().is_empty());
    let urgent = c.notify(critical("upower", "Battery at 3%"), t0, 0);
    assert_eq!(shown(&c).first(), Some(&urgent), "it shows");
    assert_eq!(live(&c), MAX_LIVE, "still at the cap");
    assert_eq!(c.take_effects(), [Effect::Closed(ids[0], CloseReason::Expired)], "the oldest made room");
    assert_eq!(c.entry(ids[0]).map(|e| e.live), Some(false), "kept in the history");
    let late = c.notify(note("late", "y"), t0, 0);
    assert!(!c.entry(late).unwrap().live, "a normal one still waits for room: history only");
    // Dismissed, then sent again within the merge window, at the cap: a bubble again, never
    // merged into the closed one.
    c.close(urgent, CloseReason::Dismissed, t0);
    c.notify(note("filler", "z"), t0, 0);
    assert_eq!(live(&c), MAX_LIVE);
    let again = c.notify(critical("upower", "Battery at 3%"), t0, 0);
    assert_ne!(again, urgent, "not merged");
    assert_eq!(shown(&c).first(), Some(&again));
}

#[test]
fn do_not_disturb_shows_critical_bubbles_only_and_spends_no_token() {
    let t0 = Instant::now();
    let mut c = Center::new(&NotificationsConfig::default(), true);
    assert!(c.dnd());
    let quiet = c.notify(note("Equibop", "Lina: hi"), t0, 0);
    assert!(c.bubbles().is_empty() && !c.entry(quiet).unwrap().live, "history only");
    assert_eq!(c.view(true), NotifyView { unread: 1, dnd: true, serving: true });
    assert_eq!(c.next_expiry(), None, "no bubble: no timer");
    let urgent = c.notify(critical("upower", "Battery at 3%"), t0, 0);
    assert_eq!(shown(&c), [urgent]);
    for i in 0..20 {
        c.notify(note("Equibop", &format!("m{i}")), t0, 0);
    }
    c.set_dnd(false);
    let after = c.notify(note("Equibop", "back"), t0, 0);
    assert!(c.entry(after).unwrap().live, "do-not-disturb spent no token");
    c.set_dnd(true);
    assert_eq!(shown(&c), [urgent], "back on: only the critical one shows");
}

/// The critical ones are filtered before `max_visible` is taken: four live normal bubbles newer
/// than two criticals never hide them.
#[test]
fn do_not_disturb_filters_before_it_takes_the_visible_ones() {
    let (mut c, t0) = (center(), Instant::now());
    let a = c.notify(critical("upower", "Battery at 5%"), t0, 0);
    let b = c.notify(critical("udiskie", "Disk failing"), t0, 0);
    for s in ["1", "2", "3", "4"] {
        c.notify(note(s, s), t0, 0);
    }
    assert_eq!(c.bubbles().len(), 4);
    c.set_dnd(true);
    assert_eq!(shown(&c), [b, a]);
}

#[test]
fn replacing_a_live_bubble_under_do_not_disturb_keeps_it_live_with_a_new_expiry() {
    let (mut c, t0) = (center(), Instant::now());
    let id = c.notify(note("spotify", "Song A"), t0, 0);
    c.set_dnd(true);
    let later = t0 + secs(3);
    assert_eq!(c.notify(Incoming { replaces_id: id, ..note("spotify", "Song B") }, later, 0), id);
    let e = c.entry(id).unwrap();
    assert!(e.live && e.summary == "Song B", "updated in place, still live: {e:?}");
    assert_eq!(c.next_expiry(), Some(later + secs(5)), "its expiry recomputed");
    assert!(shown(&c).is_empty(), "not shown while do-not-disturb is on");
    c.set_dnd(false);
    assert_eq!(shown(&c), [id]);
    assert_eq!(c.expire(later + secs(5)), [Effect::Closed(id, CloseReason::Expired)], "it still closes");
    let fresh = c.notify(note("other", "x"), later, 0);
    c.set_dnd(true);
    let replaced = c.notify(Incoming { replaces_id: fresh, ..note("other", "y") }, later, 0);
    assert!(c.entry(replaced).unwrap().live);
    let gone = c.notify(Incoming { replaces_id: 999, ..note("other", "z") }, later, 0);
    assert!(!c.entry(gone).unwrap().live, "no bubble to keep: history only");
}

#[test]
fn unread_counts_the_history_until_the_panel_opens() {
    let (mut c, t0) = (center(), Instant::now());
    assert_eq!(c.view(false), NotifyView { unread: 0, dnd: false, serving: false });
    c.notify(note("a", "1"), t0, 0);
    c.notify(note("b", "2"), t0, 0);
    c.notify(Incoming { transient: true, ..note("c", "3") }, t0, 0);
    assert_eq!(c.view(true).unread, 2, "a transient one is never unread");
    c.mark_read();
    assert_eq!(c.view(true).unread, 0);
    let id = c.notify(note("a", "4"), t0, 0);
    assert_eq!(c.view(true).unread, 1);
    c.remove(id);
    assert_eq!((c.view(true).unread, c.history().count()), (0, 2), "removed from the history");
    assert!(shown(&c).contains(&id), "its bubble stays until it goes");
    let closed = c.clear();
    assert_eq!(closed.len(), 4, "every live bubble closed as dismissed: {closed:?}");
    assert!(closed.iter().all(|e| matches!(e, Effect::Closed(_, CloseReason::Dismissed))));
    assert_eq!((c.history().count(), c.bubbles().len(), c.next_expiry()), (0, 0, None));
}

/// The portal sends an empty app name: the desktop entry's last label is shown.
#[test]
fn a_portal_notification_shows_its_desktop_entrys_last_label() {
    let (mut c, t0) = (center(), Instant::now());
    let id = c.notify(Incoming { desktop_entry: Some("org.gnome.Fractal".into()), ..note("", "Lina: hi") }, t0, 0);
    assert_eq!(c.entry(id).map(|e| (e.app.as_str(), e.identity.as_str())), Some(("Fractal", "org.gnome.Fractal")));
    let named = c.notify(Incoming { desktop_entry: Some("org.gnome.Fractal".into()), ..note("Fractal Beta", "x") }, t0, 0);
    assert_eq!(c.entry(named).map(|e| e.app.as_str()), Some("Fractal Beta"), "a name sent wins");
}

// ---- glyphs (spec § 2, Identity and glyph) ----

#[test]
fn the_glyph_comes_from_the_table_then_the_category_then_the_first_letter() {
    assert_eq!(glyph_for("firefox", None), '\u{f0239}');
    assert_eq!(glyph_for("org.telegram.desktop", Some("im.received")), '\u{f2c6}', "the app's own glyph first");
    assert_eq!(glyph_for("equibop", None), '\u{f066f}');
    assert_eq!(glyph_for("Unknown Mailer", Some("email.arrived")), EMAIL);
    assert_eq!(glyph_for("chatty", Some("im.received")), CHAT);
    assert_eq!(glyph_for("nm", Some("network.connected")), NETWORK);
    assert_eq!(glyph_for("udiskie", Some("device.added")), DEVICE);
    assert_eq!(glyph_for("fetcher", Some("transfer.complete")), DOWNLOAD);
    assert_eq!(glyph_for("org.gnome.Calendar", Some("x-gnome.reminder")), 'C', "reverse DNS: the last label's letter");
    assert_eq!(glyph_for("org.example.", None), 'O', "an empty last label: the whole name's");
    assert_eq!(glyph_for("notify-send", None), 'N');
    assert_eq!(glyph_for("élan app", None), 'É');
    assert_eq!(glyph_for("", None), icons::GENERIC, "the portal's empty name");
    assert_eq!(glyph_for("…", None), icons::GENERIC);
    assert_eq!(glyph_for("微信", None), icons::GENERIC, "a letter two cells wide never stands in for a glyph");
    assert_eq!(glyph_for("微x", None), 'X', "the first one-cell letter");
    assert_eq!(app_label("org.gnome.Fractal"), "Fractal");
    assert_eq!(app_label("firefox"), "firefox");
    assert_eq!(app_label("org.example."), "org.example.", "an empty last label: the whole name");
    assert_eq!(app_label("Unknown Mailer"), "Unknown Mailer");
    for g in [EMAIL, CHAT, NETWORK, DEVICE, DOWNLOAD] {
        assert_eq!(text_width(&g.to_string()), 1, "{g:?}");
    }
}

/// A reverse-DNS identity whose last label has no one-cell letter ("微") falls back to the first letter
/// of the whole identity.
#[test]
fn a_reverse_dns_whose_last_label_has_no_letter_takes_the_identitys_first_letter() {
    assert_eq!(glyph_for("org.example.微", None), 'O');
}

/// An empty desktop entry is no desktop entry: the identity and the display name are the app's.
#[test]
fn an_empty_desktop_entry_leaves_the_app_name_as_the_identity() {
    let mut c = center();
    let id = c.notify(Incoming { desktop_entry: Some(String::new()), ..note("Fictional", "x") }, Instant::now(), 0);
    assert_eq!(c.entry(id).map(|e| (e.identity.as_str(), e.app.as_str())), Some(("Fictional", "Fictional")));
}

fn ruled(rules: &[(&str, Option<RuleAction>, Option<RuleUrgency>)]) -> Center {
    let rules = rules.iter().map(|(app, action, urgency)| NotifyRule { app: (*app).into(), action: *action, urgency: *urgency }).collect();
    Center::new(&NotificationsConfig { rules, ..NotificationsConfig::default() }, false)
}

const MUTE: Option<RuleAction> = Some(RuleAction::Mute);
const DROP: Option<RuleAction> = Some(RuleAction::Drop);

/// A rule with an empty app (the config refuses one; `Center::new` takes it as is) must not match a
/// notification whose desktop entry is empty: an empty entry names nothing.
#[test]
fn an_empty_desktop_entry_matches_no_rule() {
    let mut c = ruled(&[("", MUTE, None)]);
    c.notify(Incoming { desktop_entry: Some(String::new()), ..note("Fictional", "x") }, Instant::now(), 0);
    assert_eq!(shown(&c).len(), 1, "not muted: the empty entry is not a name");
}

#[test]
fn a_muted_app_gets_no_bubble_but_is_kept_and_unread() {
    let mut c = ruled(&[("Chatty", MUTE, None)]);
    c.notify(note("chatty", "hi"), Instant::now(), 0);
    assert!(c.bubbles().is_empty());
    assert_eq!(c.history().count(), 1);
    assert_eq!(c.view(true).unread, 1);
}

#[test]
fn a_dropped_app_is_answered_but_never_stored() {
    let mut c = ruled(&[("Spam", DROP, None)]);
    let t = Instant::now();
    let (a, b) = (c.notify(note("Spam", "1"), t, 0), c.notify(note("Spam", "2"), t, 0));
    assert!(a != 0 && b != 0 && a != b);
    assert_eq!(c.notify(Incoming { replaces_id: 7, ..note("Spam", "3") }, t, 0), 7);
    assert_eq!(c.history().count(), 0);
    assert!(c.bubbles().is_empty());
    assert_eq!(c.view(true).unread, 0);
}

#[test]
fn an_urgency_rule_replaces_the_senders() {
    let mut c = ruled(&[("Pager", None, Some(RuleUrgency::Critical))]);
    c.set_dnd(true);
    c.notify(note("Pager", "up"), Instant::now(), 0);
    assert_eq!(c.history().next().unwrap().urgency, Urgency::Critical);
    assert_eq!(c.bubbles().len(), 1, "critical shows under do-not-disturb");
    let mut c = ruled(&[("Pager", None, Some(RuleUrgency::Low))]);
    c.notify(critical("Pager", "up"), Instant::now(), 0);
    assert_eq!(c.history().next().unwrap().urgency, Urgency::Low);
}

#[test]
fn mute_wins_over_a_critical_urgency() {
    let mut c = ruled(&[("Pager", MUTE, Some(RuleUrgency::Critical))]);
    c.notify(note("Pager", "up"), Instant::now(), 0);
    assert!(c.bubbles().is_empty());
    assert_eq!(c.history().count(), 1);
}

#[test]
fn the_first_matching_rule_wins() {
    let mut c = ruled(&[("A", MUTE, None), ("a", DROP, None)]);
    c.notify(note("A", "x"), Instant::now(), 0);
    assert_eq!(c.history().count(), 1, "stored, not dropped");
}

#[test]
fn rules_match_the_whole_name_in_any_case() {
    let mut c = ruled(&[("Fire", MUTE, None)]);
    c.notify(note("Firefox", "x"), Instant::now(), 0);
    assert_eq!(c.bubbles().len(), 1, "a part of the name is no match");
    let mut c = ruled(&[("FIREFOX", MUTE, None)]);
    c.notify(note("firefox", "x"), Instant::now(), 0);
    assert!(c.bubbles().is_empty());
}

#[test]
fn rules_match_the_desktop_entry() {
    let mut c = ruled(&[("org.example.chatty", MUTE, None)]);
    c.notify(Incoming { desktop_entry: Some("org.example.chatty".into()), ..note("", "x") }, Instant::now(), 0);
    assert!(c.bubbles().is_empty());
    assert_eq!(c.history().count(), 1);
}

#[test]
fn a_muted_duplicate_still_merges() {
    let mut c = ruled(&[("Chatty", MUTE, None)]);
    let t = Instant::now();
    c.notify(note("Chatty", "same"), t, 0);
    c.notify(note("Chatty", "same"), t + Duration::from_millis(200), 0);
    assert_eq!(c.history().count(), 1);
    assert!(c.bubbles().is_empty());
}

#[test]
fn a_dropped_flood_spends_no_token() {
    let mut c = ruled(&[("Spam", DROP, None)]);
    let t = Instant::now();
    for i in 0..20 {
        c.notify(note("Spam", &format!("s{i}")), t, 0);
    }
    for i in 0..10 {
        c.notify(note("Other", &format!("o{i}")), t, 0);
    }
    assert_eq!(c.history().count(), 10);
    assert_eq!(c.bubbles().len(), 4, "visible");
    assert_eq!(c.slots.iter().filter(|s| s.entry.live).count(), 10, "all ten got a bubble");
    assert!(c.buckets.iter().all(|b| b.identity != "Spam"), "no bucket for the dropped app");
    assert!(c.buckets.iter().any(|b| b.identity == "Other"));
}

#[test]
fn a_muted_critical_demotes_nobody() {
    let mut c = ruled(&[("Pager", MUTE, Some(RuleUrgency::Critical))]);
    let t = Instant::now();
    for i in 0..MAX_LIVE {
        c.notify(note(&format!("App{i}"), "x"), t, 0);
    }
    assert_eq!(c.slots.iter().filter(|s| s.entry.live).count(), MAX_LIVE);
    c.take_effects();
    c.notify(note("Pager", "up"), t, 0);
    assert!(c.take_effects().is_empty());
    assert_eq!(c.slots.iter().filter(|s| s.entry.live).count(), MAX_LIVE);
}

#[test]
fn a_mute_replacing_a_live_bubble_closes_it() {
    let mut c = ruled(&[("Chatty", MUTE, None)]);
    let t = Instant::now();
    // A live bubble from a name the rule does not match, then replaced under a name it does.
    let live = c.notify(note("Chat", "a"), t, 0);
    assert_eq!(c.bubbles().len(), 1);
    c.take_effects();
    assert_eq!(c.notify(Incoming { replaces_id: live, ..note("Chatty", "b") }, t, 0), live);
    assert_eq!(c.take_effects(), vec![Effect::Closed(live, CloseReason::Expired)]);
    assert!(c.bubbles().is_empty());
}

#[test]
fn a_subdivision_flag_survives_in_the_title_and_the_body() {
    let flag = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}";
    assert_eq!(summary_text(&format!("Match {flag}")), format!("Match {flag}"));
    assert_eq!(body_text(&format!("{flag} wins")), format!("{flag} wins"));
}
