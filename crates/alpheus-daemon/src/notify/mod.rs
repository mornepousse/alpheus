//! The notification server's model: what a client sent, cleaned and capped, and which of it shows.
//!
//! This is the pure half of the notification server. The cleaners (`app_text`, `summary_text`,
//! `body_text`, `hint_text`, `action`) turn a client's raw fields into capped plain text, and
//! `Center` holds everything after that: ids, tags and revisions, the timeouts, the spam guards
//! (a 1 s merge window, a token bucket per application, a cap on live bubbles), the history,
//! do-not-disturb, the unread count, the rules of `[[notifications.rules]]`, and which bubbles
//! show. The D-Bus side decodes a call in `session_bus/codec.rs` and calls the cleaners here;
//! `app.rs` is the daemon's side (the bell, do-not-disturb, Clear all, the `alpheus-launch`
//! verbs), `bubbles.rs` and `bubbles_surface.rs` the bubbles, `panel.rs` and `panel_app.rs` the
//! panel, `dnd.rs` and `battery_alert.rs` two small models.
//!
//! Never:
//! - name D-Bus, or start anything (a thread, a process, a timer);
//! - read the clock: the callers pass `now` (an `Instant`) and `at` (the local time in minutes,
//!   `clock::local_minutes`);
//! - write a log line (a guardrail, `the_notification_rules_log_nothing`, reads this file);
//! - panic (a guardrail, `the_notification_server_has_no_panicking_path`);
//! - hold more than `history + MAX_LIVE` entries, each field capped.
//!
//! See: docs/specs/2026-10-09-alpheus-notifications-design.md, docs/features/notifications.md, and
//! docs/decisions.md ("A bubble's timeout follows its urgency", "A notification gets a bubble only
//! when all three guards allow it", "A critical bubble always shows, and the criticals are capped",
//! "A bubble's timeout starts when it first shows", "An identical notification within one second is
//! merged, unless it would add a bubble", "A notification's text is cleaned in one linear pass",
//! "An unknown `replaces_id` is accepted as that id", "A rule is matched on the whole app name, any
//! case").

use crate::config::{NotificationsConfig, NotifyRule, RuleAction, RuleUrgency};
use crate::grid::text_width;
use crate::icons;
use crate::text::invisible;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Bytes kept of each field (spec § 2), cut on a character boundary.
pub const MAX_APP: usize = 64;
pub const MAX_SUMMARY: usize = 256;
pub const MAX_BODY: usize = 1024;
/// Actions kept; bytes of a label; bytes of a key (sent back as is: a longer one is refused, never cut).
pub const MAX_ACTIONS: usize = 6;
pub const MAX_LABEL: usize = 64;
pub const MAX_KEY: usize = 64;
/// Bytes kept of a string hint: desktop-entry, category, the synchronous tag.
pub const MAX_HINT: usize = 128;
/// Bytes of a raw field looked at before any cleaning: the rest of a hostile body is never read.
pub const RAW_MAX: usize = 16 * 1024;
/// An identical notification arriving within this long of the last one is merged into it; the
/// window slides with each merge.
pub const MERGE_WINDOW: Duration = Duration::from_secs(1);
/// Each identity's token bucket: this many bubbles at once, then one more per whole second.
pub const BUCKET: u32 = 10;
/// Live notifications at most (bubbles shown or waiting): beyond, history only; a critical one
/// makes room (the oldest normal bubble goes).
pub const MAX_LIVE: usize = 32;
/// Identities with a bucket at most; a full bucket is forgotten.
pub const MAX_BUCKETS: usize = 64;

/// Category glyphs (spec § 2), checked in JetBrainsMono Nerd Font Mono 3.4.0's post table.
pub const EMAIL: char = '\u{f01ee}'; // md-email
pub const CHAT: char = '\u{f0369}'; // md-message_text
pub const NETWORK: char = '\u{f0317}'; // md-lan
pub const DEVICE: char = '\u{f0fb0}'; // md-devices
pub const DOWNLOAD: char = '\u{f01da}'; // md-download
/// (category prefix, glyph), first match wins.
const CATEGORIES: [(&str, char); 5] =
    [("email.", EMAIL), ("im.", CHAT), ("network.", NETWORK), ("device.", DEVICE), ("transfer.complete", DOWNLOAD)];
/// The five XML entities, decoded once.
const ENTITIES: [(&str, char); 5] = [("&amp;", '&'), ("&lt;", '<'), ("&gt;", '>'), ("&quot;", '"'), ("&apos;", '\'')];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    /// The `urgency` hint (a byte, or any integer a client sends instead), clamped to 0..=2.
    pub fn from_level(n: i64) -> Urgency {
        match n {
            ..=0 => Urgency::Low,
            1 => Urgency::Normal,
            _ => Urgency::Critical,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub key: String,
    pub label: String,
}

/// What the codec decodes from a Notify call (already capped and cleaned by the `*_text` cleaners).
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub app_name: String,
    pub replaces_id: u32,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    pub urgency: Urgency,
    pub expire_ms: i32,
    pub desktop_entry: Option<String>,
    pub category: Option<String>,
    pub transient: bool,
    pub resident: bool,
    /// `x-canonical-private-synchronous`.
    pub tag: Option<String>,
}

/// One notification, as the bubbles and the panel show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    /// Bumped by each replacement: a click carries the revision it saw.
    pub revision: u32,
    /// `desktop-entry`, else the app name: keys the bucket, the merge window, the tag and the glyph.
    pub identity: String,
    /// The name shown: the app name, else the identity.
    pub app: String,
    pub glyph: char,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    pub urgency: Urgency,
    /// When it arrived, in local minutes since 1970 (`clock::local_minutes`): "14:02" today,
    /// "Oct 8" on another day.
    pub at: u32,
    /// It has a bubble (shown or waiting).
    pub live: bool,
    pub read: bool,
    pub resident: bool,
}

/// `NotificationClosed`'s reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

/// What the D-Bus side must emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Closed(u32, CloseReason),
    Invoked(u32, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub default: Duration,
    pub min: Duration,
    pub max: Duration,
}

impl Timeouts {
    pub fn of(cfg: &NotificationsConfig) -> Timeouts {
        let s = |n: u32| Duration::from_secs(n.into());
        Timeouts { default: s(cfg.timeout), min: s(cfg.timeout_min), max: s(cfg.timeout_max) }
    }
}

/// How long a bubble stays: None (sticky) when critical; a negative `expire_ms` is the default, 0
/// is `max`, any other is clamped to `min..=max`, whatever the bounds hold, even `min` above `max`
/// (the config bounds are not compared), so this never panics. See docs/decisions.md, "A bubble's
/// timeout follows its urgency".
pub fn timeout(urgency: Urgency, expire_ms: i32, t: &Timeouts) -> Option<Duration> {
    if urgency == Urgency::Critical {
        return None;
    }
    Some(match expire_ms {
        ..0 => t.default,
        0 => t.max,
        // `.max(t.min).min(t.max)` rather than `clamp`: `clamp` panics when min is above max.
        ms => Duration::from_millis(u64::from(ms.unsigned_abs())).max(t.min).min(t.max),
    })
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Step back from `max` to the start of the character that holds it, so no character is split.
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `s` cut to at most `max` bytes, never inside a character.
pub fn cap_bytes(s: &str, max: usize) -> String {
    cut(s, max).to_string()
}

/// Trimmed, cut to `max` bytes, trimmed again (the cut may end on a space).
fn fit(s: &str, max: usize) -> String {
    cut(s.trim(), max).trim_end().to_string()
}

/// Controls and invisible/bidi formatting characters removed; `\r` dropped; `\t` a space; `\n`
/// and the Unicode line and paragraph separators a line break when `multiline`, a space otherwise.
fn clean(s: &str, multiline: bool) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\u{2028}' | '\u{2029}' if multiline => Some('\n'),
            '\n' | '\u{2028}' | '\u{2029}' | '\t' => Some(' '),
            '\r' => None,
            c if c.is_control() || invisible(c) => None,
            c => Some(c),
        })
        .collect()
}

/// Where a tag that starts at `at` (on its `<`) ends.
enum TagEnd {
    /// Just past its `>`.
    At(usize),
    /// A `<` came first: the opening `<` was text.
    Broken,
    /// The input ended inside it.
    Unclosed,
}

/// Whether a whole tag (`<` to `>`) is a line break: its first token, cut at whitespace, `/` or
/// the `>`, is `br` in any case (`<br>`, `<br/>`, `<br / >`, `<br clear="all">`).
fn is_br(tag: &str) -> bool {
    let name = tag[1..].split(|c: char| c.is_whitespace() || matches!(c, '/' | '>')).next().unwrap_or_default();
    name.eq_ignore_ascii_case("br")
}

/// Where the tag that starts at the beginning of `at` ends. A quote opens a quoted part in which
/// `>` and `<` are plain characters, until the same quote closes it.
fn tag_end(at: &str) -> TagEnd {
    let mut quote = None;
    for (i, c) in at.char_indices().skip(1) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '>') => return TagEnd::At(i + 1),
            (None, '<') => return TagEnd::Broken,
            (None, '"' | '\'') => quote = Some(c),
            (None, _) => {}
        }
    }
    TagEnd::Unclosed
}

/// Markup to plain text (spec § 2), in one linear pass: a tag (`<` then an ASCII letter, `/`, `!`
/// or `?`, up to its `>` outside quotes) is removed, its content kept, a `<br>` becoming `\n`; any
/// other `<` is text, and so is a tag left unclosed, after which every `<` is text (no second
/// scan). The five XML entities are decoded once; anything else after `&` stays as sent.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut tags_close = true;
    while let Some(i) = rest.find(['<', '&']) {
        out.push_str(&rest[..i]);
        let at = &rest[i..];
        if let Some(after) = at.strip_prefix('&') {
            match ENTITIES.iter().find(|(name, _)| at.starts_with(*name)) {
                Some((name, c)) => {
                    out.push(*c);
                    rest = &at[name.len()..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            }
            continue;
        }
        let opens = at[1..].starts_with(|c: char| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
        let end = if opens && tags_close { tag_end(at) } else { TagEnd::Broken };
        match end {
            TagEnd::At(n) => {
                if is_br(&at[..n]) {
                    out.push('\n');
                }
                rest = &at[n..];
            }
            other => {
                if matches!(other, TagEnd::Unclosed) {
                    tags_close = false;
                }
                out.push('<');
                rest = &at[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// An app name: one cleaned line, trimmed, at most MAX_APP bytes (never parsed as markup).
///
/// `cut(s, RAW_MAX)` comes first in every cleaner below: the rest of a hostile field is never
/// looked at.
pub fn app_text(s: &str) -> String {
    fit(&clean(cut(s, RAW_MAX), false), MAX_APP)
}

/// A summary: markup stripped, one cleaned line, trimmed, at most MAX_SUMMARY bytes.
pub fn summary_text(s: &str) -> String {
    fit(&clean(&strip_markup(cut(s, RAW_MAX)), false), MAX_SUMMARY)
}

/// A body: markup stripped, cleaned with its line breaks kept, trimmed, at most MAX_BODY bytes.
pub fn body_text(s: &str) -> String {
    fit(&clean(&strip_markup(cut(s, RAW_MAX)), true), MAX_BODY)
}

/// A string hint (desktop-entry, category, the synchronous tag): one cleaned line, trimmed, at most
/// MAX_HINT bytes; None when blank.
pub fn hint_text(s: &str) -> Option<String> {
    let t = fit(&clean(cut(s, RAW_MAX), false), MAX_HINT);
    if t.is_empty() { None } else { Some(t) }
}

/// An action: its key as sent (1 to MAX_KEY bytes, no control character: it goes back over D-Bus
/// unchanged, so it is refused rather than cut), its label one cleaned line of at most MAX_LABEL bytes.
pub fn action(key: &str, label: &str) -> Option<Action> {
    let sane = !key.is_empty() && key.len() <= MAX_KEY && !key.chars().any(char::is_control);
    if !sane {
        return None;
    }
    Some(Action { key: key.to_string(), label: fit(&clean(cut(label, RAW_MAX), false), MAX_LABEL) })
}

/// The first alphanumeric character of `s` that takes exactly one cell (never a lone combining
/// mark, never a wide one such as '微': the glyph has one cell), upper-cased.
fn first_letter(s: &str) -> Option<char> {
    for c in s.chars() {
        // `encode_utf8` needs a buffer to write the character into; `text_width` measures it in cells.
        if c.is_alphanumeric() && text_width(c.encode_utf8(&mut [0; 4])) == 1 {
            return c.to_uppercase().next();
        }
    }
    None
}

/// A notification's glyph (spec § 2): the bar's table by identity, then the category's glyph, then
/// the identity's first letter (a reverse-DNS identity's last label first), then GENERIC.
pub fn glyph_for(identity: &str, category: Option<&str>) -> char {
    if let Some(glyph) = icons::find(identity) {
        return glyph;
    }
    if let Some(category) = category {
        for (prefix, glyph) in CATEGORIES {
            if category.starts_with(prefix) {
                return glyph;
            }
        }
    }
    if let Some(letter) = first_letter(app_label(identity)) {
        return letter;
    }
    if let Some(letter) = first_letter(identity) {
        return letter;
    }
    icons::GENERIC
}

/// The name shown for `identity` when the app sent none (the portal's): a reverse-DNS desktop
/// entry's last label ("Fractal" for org.gnome.Fractal), else the identity as it is.
pub fn app_label(identity: &str) -> &str {
    match identity.rsplit('.').next() {
        Some(label) if !label.is_empty() && !identity.contains(char::is_whitespace) => label,
        _ => identity,
    }
}

/// What the bell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifyView {
    pub unread: u16,
    pub dnd: bool,
    /// The name is ours (another server, mako, holds it otherwise).
    pub serving: bool,
}

/// One bubble to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown<'a> {
    pub entry: &'a Entry,
    pub critical: bool,
}

/// An entry and what only the model needs about it (the `Entry` is what the bubbles and the panel
/// get to see).
#[derive(Debug, Clone)]
struct Slot {
    entry: Entry,
    tag: Option<String>,
    /// When it last arrived (the merge window slides).
    arrived: Instant,
    /// How long its bubble stays once shown: Some only while live with a finite timeout.
    life: Option<Duration>,
    /// When its bubble expires: armed (`Center::arm`) the first time it is among the bubbles that
    /// show, do-not-disturb aside; None while it waits beyond `max_visible`, and for a sticky one.
    expiry: Option<Instant>,
    /// The host drew it with its timer running (`Center::seen`): pushed back behind newer bubbles,
    /// it keeps its expiry.
    shown: bool,
    /// In the history (not transient, and within the cap).
    kept: bool,
}

#[derive(Debug, Clone)]
struct Bucket {
    identity: String,
    tokens: u32,
    since: Instant,
}

impl Bucket {
    /// The tokens it holds at `now`: one more per whole second since `since`, up to BUCKET.
    /// `saturating_add` stops at the largest number instead of wrapping round.
    fn tokens_at(&self, now: Instant) -> u32 {
        let secs = now.saturating_duration_since(self.since).as_secs();
        self.tokens.saturating_add(u32::try_from(secs).unwrap_or(u32::MAX)).min(BUCKET)
    }

    /// Refilled to `now`; a full bucket restarts its clock.
    fn refill(&mut self, now: Instant) {
        let secs = now.saturating_duration_since(self.since).as_secs();
        if secs == 0 {
            return;
        }
        self.tokens = self.tokens_at(now);
        self.since = if self.tokens == BUCKET { now } else { self.since + Duration::from_secs(secs) };
    }

    /// Spends one token (after a refill) and says whether there was one.
    fn take(&mut self, now: Instant) -> bool {
        self.refill(now);
        let some = self.tokens > 0;
        if some {
            self.tokens -= 1;
        }
        some
    }
}

/// The id after `id`: never 0 (u32::MAX wraps to 1).
fn next_after(id: u32) -> u32 {
    id.checked_add(1).unwrap_or(1)
}

/// A `[[notifications.rules]]` entry as the center keeps it: the app lowercased once.
#[derive(Debug, Clone)]
struct Rule {
    app: String,
    action: Option<RuleAction>,
    urgency: Option<Urgency>,
}

impl Rule {
    fn of(r: &NotifyRule) -> Rule {
        let urgency = r.urgency.map(|u| match u {
            RuleUrgency::Low => Urgency::Low,
            RuleUrgency::Normal => Urgency::Normal,
            RuleUrgency::Critical => Urgency::Critical,
        });
        Rule { app: r.app.to_lowercase(), action: r.action, urgency }
    }
}

/// The notifications: the history, the live bubbles and their expiries, the id counter, the tags,
/// the buckets, do-not-disturb, the unread count.
#[derive(Debug, Clone)]
pub struct Center {
    /// Newest first.
    slots: VecDeque<Slot>,
    next_id: u32,
    buckets: Vec<Bucket>,
    dnd: bool,
    timeouts: Timeouts,
    max_visible: usize,
    history: usize,
    /// What `notify` itself closed (a critical demoted), for the D-Bus side (`take_effects`).
    effects: Vec<Effect>,
    /// `[[notifications.rules]]`, first match wins.
    rules: Vec<Rule>,
}

impl Center {
    pub fn new(cfg: &NotificationsConfig, dnd: bool) -> Center {
        Center {
            slots: VecDeque::new(),
            next_id: 1,
            buckets: Vec::new(),
            dnd,
            timeouts: Timeouts::of(cfg),
            max_visible: usize::from(cfg.max_visible),
            history: usize::from(cfg.history),
            effects: Vec::new(),
            rules: cfg.rules.iter().map(Rule::of).collect(),
        }
    }

    /// The first rule whose app is the whole name of the sender or its desktop entry, any case.
    fn rule_for(&self, n: &Incoming) -> Option<&Rule> {
        // With no rule, nothing is lowercased.
        if self.rules.is_empty() {
            return None;
        }
        let entry = match n.desktop_entry.as_deref() {
            Some(d) if !d.is_empty() => Some(d.to_lowercase()),
            _ => None,
        };
        let name = n.app_name.to_lowercase();
        self.rules.iter().find(|r| r.app == name || entry.as_deref() == Some(r.app.as_str()))
    }

    fn position(&self, id: u32) -> Option<usize> {
        self.slots.iter().position(|s| s.entry.id == id)
    }

    fn live_count(&self) -> usize {
        self.slots.iter().filter(|s| s.entry.live).count()
    }

    /// A fresh id: the counter's, skipping ids still held (after a wrap); never 0.
    fn fresh_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = next_after(id);
            if self.position(id).is_none() {
                return id;
            }
        }
    }

    /// An unknown `replaces_id` taken as is: the counter jumps past it, never back.
    fn reserve(&mut self, id: u32) {
        if id >= self.next_id {
            self.next_id = next_after(id);
        }
    }

    /// A token from `identity`'s bucket. A new identity gets a bucket once the full ones are
    /// forgotten and fewer than MAX_BUCKETS remain; otherwise none.
    fn take_token(&mut self, identity: &str, now: Instant) -> bool {
        let i = match self.buckets.iter().position(|b| b.identity == identity) {
            Some(i) => i,
            None => {
                self.buckets.retain_mut(|b| {
                    b.refill(now);
                    b.tokens < BUCKET
                });
                if self.buckets.len() >= MAX_BUCKETS {
                    return false;
                }
                self.buckets.push(Bucket { identity: identity.to_string(), tokens: BUCKET, since: now });
                self.buckets.len() - 1
            }
        };
        self.buckets[i].take(now)
    }

    /// Whether `take_token` would grant one, without spending it nor forgetting any bucket.
    fn has_token(&self, identity: &str, now: Instant) -> bool {
        match self.buckets.iter().find(|b| b.identity == identity) {
            Some(b) => b.tokens_at(now) > 0,
            None => self.buckets.iter().filter(|b| b.tokens_at(now) < BUCKET).count() < MAX_BUCKETS,
        }
    }

    /// Whether a new notification (replacing nothing) would get a bubble, without spending a token.
    fn would_show(&self, identity: &str, critical: bool, now: Instant) -> bool {
        critical || (!self.dnd && self.live_count() < MAX_LIVE && self.has_token(identity, now))
    }

    /// Entries beyond the history cap leave it (newest kept); entries neither kept nor live go.
    fn trim(&mut self) {
        let cap = self.history;
        let mut kept = 0;
        for s in &mut self.slots {
            if s.kept {
                kept += 1;
                s.kept = kept <= cap;
            }
        }
        self.slots.retain(|s| s.kept || s.entry.live);
    }

    /// Takes in one notification and returns its id: always answered, bubble or not (see the
    /// notifications spec for replacement, tags, merging, the bucket and the history).
    pub fn notify(&mut self, mut n: Incoming, now: Instant, at: u32) -> u32 {
        // The rules come first: a drop is answered and forgotten, an urgency replaces the sender's,
        // a mute leaves the bubble out of everything below (the merge included).
        let (action, urgency) = match self.rule_for(&n) {
            Some(r) => (r.action, r.urgency),
            None => (None, None),
        };
        if action == Some(RuleAction::Drop) {
            return if n.replaces_id != 0 { n.replaces_id } else { self.fresh_id() };
        }
        let muted = action == Some(RuleAction::Mute);
        if let Some(u) = urgency {
            n.urgency = u;
        }
        let identity = match n.desktop_entry.as_deref() {
            Some(d) if !d.is_empty() => d.to_string(),
            _ => n.app_name.clone(),
        };
        let critical = n.urgency == Urgency::Critical;
        let target = if n.replaces_id != 0 {
            self.position(n.replaces_id)
        } else {
            match n.tag.as_deref() {
                Some(tag) => self.slots.iter().position(|s| s.tag.as_deref() == Some(tag) && s.entry.identity == identity),
                None => None,
            }
        };
        // A merge never swallows a higher urgency, nor a bubble the first one never got (do-not-
        // disturb, the bucket) while this one would. The condition below is a chain: every line
        // must hold, and `let Some(i) = ...` in the middle names the entry this one merges into,
        // for the lines after it and for the body.
        if n.replaces_id == 0
            && target.is_none()
            && let Some(i) = self.slots.iter().position(|s| {
                s.entry.identity == identity
                    && s.entry.summary == n.summary
                    && s.entry.body == n.body
                    && s.entry.urgency == n.urgency
                    && now.saturating_duration_since(s.arrived) < MERGE_WINDOW
            })
            && (self.slots[i].entry.live || muted || !self.would_show(&identity, critical, now))
        {
            self.slots[i].arrived = now;
            let id = self.slots[i].entry.id;
            // A live bubble still waiting beyond `max_visible` comes to the top, as a fresh one would,
            // with the copy's time: the history stays newest first, its times in order.
            let waiting = self.shows(&self.slots[i]) && !self.bubbles().iter().any(|b| b.entry.id == id);
            if waiting && let Some(mut slot) = self.slots.remove(i) {
                slot.entry.at = at;
                self.slots.push_front(slot);
            }
            self.arm(now);
            return id;
        }
        // A live bubble replaced stays live under do-not-disturb (updated in place, its expiry
        // recomputed); it only shows once do-not-disturb is off, unless critical.
        let had_bubble = target.is_some_and(|i| self.slots[i].entry.live);
        // The three guards run in this order, so that do-not-disturb and a full screen spend no
        // token (docs/decisions.md, "A notification gets a bubble only when all three guards
        // allow it"). The live cap never blocks a critical one: the oldest live normal bubble
        // makes room (docs/decisions.md, "A critical bubble always shows, and the criticals are capped").
        let bubble = !muted && (had_bubble || critical || (!self.dnd && self.live_count() < MAX_LIVE && self.take_token(&identity, now)));
        if critical && bubble && !had_bubble && self.live_count() >= MAX_LIVE {
            self.demote_oldest_normal();
        }
        // `remove` gives the old entry back, so its id is kept and its revision goes up by one.
        let replaced = match target {
            Some(i) => self.slots.remove(i),
            None => None,
        };
        let (id, revision) = match replaced {
            Some(old) => (old.entry.id, old.entry.revision.wrapping_add(1)),
            None if n.replaces_id != 0 => {
                self.reserve(n.replaces_id);
                (n.replaces_id, 1)
            }
            None => (self.fresh_id(), 1),
        };
        let glyph = glyph_for(&identity, n.category.as_deref());
        let life = if bubble { timeout(n.urgency, n.expire_ms, &self.timeouts) } else { None };
        let kept = !n.transient && self.history > 0;
        let app = if n.app_name.is_empty() { app_label(&identity).to_string() } else { n.app_name };
        // A mute ends the bubble it replaces: the host is told, as for a demotion.
        if muted && had_bubble {
            self.effects.push(Effect::Closed(id, CloseReason::Expired));
        }
        let entry = Entry {
            id,
            revision,
            identity,
            app,
            glyph,
            summary: n.summary,
            body: n.body,
            actions: n.actions,
            urgency: n.urgency,
            at,
            live: bubble,
            read: false,
            resident: n.resident,
        };
        self.slots.push_front(Slot { entry, tag: n.tag, arrived: now, life, expiry: None, shown: false, kept });
        if critical && bubble {
            self.demote_criticals();
        }
        self.trim();
        self.arm(now);
        id
    }

    /// The oldest live bubble that is not critical goes to the history, closed as expired (the live
    /// cap reached by a critical one; criticals are at most `max_visible` × 2 ≤ 16 of the 32).
    fn demote_oldest_normal(&mut self) {
        if let Some(s) = self.slots.iter_mut().rev().find(|s| s.entry.live && s.entry.urgency != Urgency::Critical) {
            s.entry.live = false;
            s.expiry = None;
            self.effects.push(Effect::Closed(s.entry.id, CloseReason::Expired));
        }
    }

    /// Live criticals beyond `max_visible` × 2 (a flood never blocks a real one): the oldest go to
    /// the history, closed as expired.
    fn demote_criticals(&mut self) {
        let cap = self.max_visible.saturating_mul(2);
        let mut seen = 0;
        for s in &mut self.slots {
            if s.entry.live && s.entry.urgency == Urgency::Critical {
                seen += 1;
                if seen > cap {
                    s.entry.live = false;
                    s.expiry = None;
                    self.effects.push(Effect::Closed(s.entry.id, CloseReason::Expired));
                }
            }
        }
    }

    /// Live slots in the order the bubbles show, do-not-disturb aside: the critical ones first,
    /// then the others, newest first within each.
    fn queue(&self) -> impl Iterator<Item = &Slot> {
        let live = |critical: bool| self.slots.iter().filter(move |s| s.entry.live && (s.entry.urgency == Urgency::Critical) == critical);
        live(true).chain(live(false))
    }

    /// The expiries after a change to the bubbles, from `now` (spec § 3, extras wait; docs/decisions.md,
    /// "A bubble's timeout starts when it first shows"): each bubble
    /// that shows, do-not-disturb aside (a bubble it hides keeps its timer), is armed when it has no
    /// expiry yet. One pushed back beyond `max_visible` before the host drew it (`seen`: six at
    /// once in one batch) waits again, disarmed, and gets its whole timeout when it shows; one the
    /// host drew keeps its expiry, so a steady flow never stops the bubbles from expiring.
    fn arm(&mut self, now: Instant) {
        let mut room = self.max_visible;
        for critical in [true, false] {
            for s in self.slots.iter_mut().filter(|s| s.entry.live && (s.entry.urgency == Urgency::Critical) == critical) {
                if room == 0 {
                    if !s.shown {
                        s.expiry = None;
                    }
                } else {
                    room -= 1;
                    if s.expiry.is_none() {
                        // `checked_add` gives None instead of overflowing an `Instant`.
                        s.expiry = match s.life {
                            Some(d) => now.checked_add(d),
                            None => None,
                        };
                    }
                }
            }
        }
    }

    /// The host drew the bubbles (`Notifications::take_bubbles_change`, after each batch of calls
    /// and each act): every bubble whose timer runs has shown and keeps it from now on.
    pub fn seen(&mut self) {
        for s in &mut self.slots {
            s.shown |= s.expiry.is_some();
        }
    }

    /// Ends `id`'s bubble: `Closed(id, reason)` when it was live. `Dismissed` marks it read;
    /// `Closed` (the app's call) removes it from the history too; a transient one goes.
    pub fn close(&mut self, id: u32, reason: CloseReason, now: Instant) -> Vec<Effect> {
        let Some(i) = self.position(id) else { return Vec::new() };
        let s = &mut self.slots[i];
        // `replace` sets `live` to false and gives back what it was.
        let was_live = std::mem::replace(&mut s.entry.live, false);
        s.expiry = None;
        if reason == CloseReason::Dismissed {
            s.entry.read = true;
        }
        if reason == CloseReason::Closed || !s.kept {
            self.slots.remove(i);
        }
        self.arm(now);
        if was_live { vec![Effect::Closed(id, reason)] } else { Vec::new() }
    }

    /// Runs `key` on `id`: only live, at its current `revision`, with a key it offered (a stale
    /// revision or anything else: nothing). A resident entry stays (read); any other is removed and
    /// closed as dismissed.
    pub fn invoke(&mut self, id: u32, revision: u32, key: &str, now: Instant) -> Vec<Effect> {
        let Some(i) = self.position(id) else { return Vec::new() };
        let e = &mut self.slots[i].entry;
        if !e.live || e.revision != revision || !e.actions.iter().any(|a| a.key == key) {
            return Vec::new();
        }
        let mut out = vec![Effect::Invoked(id, key.to_string())];
        if e.resident {
            e.read = true;
        } else {
            self.slots.remove(i);
            out.push(Effect::Closed(id, CloseReason::Dismissed));
            self.arm(now);
        }
        out
    }

    /// Every bubble whose time is up at `now`: `Closed(id, Expired)`; its entry stays in the history.
    pub fn expire(&mut self, now: Instant) -> Vec<Effect> {
        let mut out = Vec::new();
        for s in &mut self.slots {
            if s.expiry.is_some_and(|at| at <= now) {
                s.expiry = None;
                s.entry.live = false;
                out.push(Effect::Closed(s.entry.id, CloseReason::Expired));
            }
        }
        self.slots.retain(|s| s.kept || s.entry.live);
        self.arm(now);
        out
    }

    /// What `notify` closed on its own (a critical demoted by a flood), taken.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The earliest expiry over the live notifications; None at rest or with sticky ones only.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.slots.iter().filter_map(|s| s.expiry).min()
    }

    /// Whether `s` is a bubble `bubbles` may draw: live, and critical under do-not-disturb.
    fn shows(&self, s: &Slot) -> bool {
        s.entry.live && (!self.dnd || s.entry.urgency == Urgency::Critical)
    }

    /// The bubbles to draw: live, critical first, newest first within each, at most `max_visible`;
    /// under do-not-disturb, critical only.
    pub fn bubbles(&self) -> Vec<Shown<'_>> {
        self.queue()
            .filter(|s| self.shows(s))
            .take(self.max_visible)
            .map(|s| Shown { entry: &s.entry, critical: s.entry.urgency == Urgency::Critical })
            .collect()
    }

    /// The history, newest first.
    pub fn history(&self) -> impl Iterator<Item = &Entry> {
        self.slots.iter().filter(|s| s.kept).map(|s| &s.entry)
    }

    pub fn entry(&self, id: u32) -> Option<&Entry> {
        self.position(id).map(|i| &self.slots[i].entry)
    }

    /// Out of the history (the panel's Delete); a bubble it still has stays until it closes.
    pub fn remove(&mut self, id: u32) {
        if let Some(i) = self.position(id) {
            self.slots[i].kept = false;
            self.slots.retain(|s| s.kept || s.entry.live);
        }
    }

    /// The server stopped serving (NameLost, or the session bus gone): every
    /// bubble goes at once, with no NotificationClosed (nobody listens to us any more), and the
    /// history stays for the panel. Nothing is left to expire: `next_expiry` is None.
    pub fn hide_all(&mut self) {
        for s in &mut self.slots {
            s.entry.live = false;
            s.expiry = None;
        }
        self.slots.retain(|s| s.kept);
    }

    /// Everything gone ("Clear all"): each live bubble closed as dismissed.
    pub fn clear(&mut self) -> Vec<Effect> {
        let out = self.slots.iter().filter(|s| s.entry.live).map(|s| Effect::Closed(s.entry.id, CloseReason::Dismissed)).collect();
        self.slots.clear();
        out
    }

    /// Opening the panel: everything read.
    pub fn mark_read(&mut self) {
        for s in &mut self.slots {
            s.entry.read = true;
        }
    }

    pub fn set_dnd(&mut self, on: bool) {
        self.dnd = on;
    }

    pub fn dnd(&self) -> bool {
        self.dnd
    }

    /// The bell: unread entries of the history, do-not-disturb, and whether the name is ours.
    pub fn view(&self, serving: bool) -> NotifyView {
        let unread = self.slots.iter().filter(|s| s.kept && !s.entry.read).count();
        // `try_from` fails only above `u16::MAX`, which the history (200 entries at most) cannot
        // reach; the count would then saturate.
        NotifyView { unread: u16::try_from(unread).unwrap_or(u16::MAX), dnd: self.dnd, serving }
    }
}

pub mod app;
pub mod battery_alert;
pub mod bubbles;
pub mod bubbles_surface;
pub mod dnd;
pub mod panel;
pub mod panel_app;

#[cfg(test)]
mod tests;
