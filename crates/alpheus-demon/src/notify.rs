//! The notification server's model (notifications spec docs/specs/2026-10-09-alpheus-notifications-design.md):
//! what came in, cleaned and capped; ids, tags and revisions; the timeouts; the spam guards (a 1 s
//! merge window, a token bucket per application, a cap on live bubbles); the history, do-not-disturb
//! and the unread count; which bubbles show. Pure: it never names D-Bus (tray/codec.rs decodes the
//! calls and calls the `*_text` cleaners here), starts nothing and never reads the clock (its callers
//! pass the `Instant` and the minute of the day). Bounded: `history + MAX_LIVE` entries, each capped.

use crate::config::NotificationsConfig;
use crate::grid::text_width;
use crate::icons;
use crate::tray::codec::invisible;
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
/// Live notifications at most (bubbles shown or waiting): beyond, history only.
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
    /// Minutes since local midnight, for "14:02".
    pub at_minute: u16,
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

/// How long a bubble stays (Mae's ruling): None (sticky) when critical; a negative `expire_ms` is
/// the default, 0 is `max`, any other is clamped to `min..=max` (never a panic, whatever they hold).
pub fn timeout(urgency: Urgency, expire_ms: i32, t: &Timeouts) -> Option<Duration> {
    if urgency == Urgency::Critical {
        return None;
    }
    Some(match expire_ms {
        ..0 => t.default,
        0 => t.max,
        ms => Duration::from_millis(u64::from(ms.unsigned_abs())).max(t.min).min(t.max),
    })
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
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

/// Whether a whole tag (`<` to `>`) is a line break: `<br>`, `<br/>`, `<br />`, any case.
fn is_br(tag: &str) -> bool {
    let name = tag[1..].trim_end_matches('>').trim_end_matches('/').trim_end();
    name.eq_ignore_ascii_case("br")
}

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
    (!t.is_empty()).then_some(t)
}

/// An action: its key as sent (1 to MAX_KEY bytes, no control character: it goes back over D-Bus
/// unchanged, so it is refused rather than cut), its label one cleaned line of at most MAX_LABEL bytes.
pub fn action(key: &str, label: &str) -> Option<Action> {
    let sane = !key.is_empty() && key.len() <= MAX_KEY && !key.chars().any(char::is_control);
    sane.then(|| Action { key: key.to_string(), label: fit(&clean(cut(label, RAW_MAX), false), MAX_LABEL) })
}

/// The first alphanumeric character of `s` that takes exactly one cell (never a lone combining
/// mark, never a wide one such as '微': the glyph has one cell), upper-cased.
fn first_letter(s: &str) -> Option<char> {
    s.chars().find(|c| c.is_alphanumeric() && text_width(c.encode_utf8(&mut [0; 4])) == 1).and_then(|c| c.to_uppercase().next())
}

/// A notification's glyph (spec § 2): the bar's table by identity, then the category's glyph, then
/// the identity's first letter (a reverse-DNS identity's last label first), then GENERIC.
pub fn glyph_for(identity: &str, category: Option<&str>) -> char {
    let label = if identity.contains(char::is_whitespace) { identity } else { identity.rsplit('.').next().unwrap_or(identity) };
    icons::find(identity)
        .or_else(|| category.and_then(|c| CATEGORIES.iter().find(|(p, _)| c.starts_with(*p)).map(|(_, g)| *g)))
        .or_else(|| first_letter(label))
        .or_else(|| first_letter(identity))
        .unwrap_or(icons::GENERIC)
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

/// An entry and what only the model needs about it.
#[derive(Debug, Clone)]
struct Slot {
    entry: Entry,
    tag: Option<String>,
    /// When it last arrived (the merge window slides).
    arrived: Instant,
    /// When its bubble expires: Some only while live with a finite timeout.
    expiry: Option<Instant>,
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
        }
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
        (!self.dnd || critical) && self.live_count() < MAX_LIVE && (critical || self.has_token(identity, now))
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
    /// decisions of the plan's Task 1 for replacement, tags, merging, the bucket and the history).
    pub fn notify(&mut self, n: Incoming, now: Instant, minute: u16) -> u32 {
        let identity = n.desktop_entry.as_deref().filter(|d| !d.is_empty()).unwrap_or(&n.app_name).to_string();
        let critical = n.urgency == Urgency::Critical;
        let target = if n.replaces_id != 0 {
            self.position(n.replaces_id)
        } else {
            n.tag.as_deref().and_then(|tag| self.slots.iter().position(|s| s.tag.as_deref() == Some(tag) && s.entry.identity == identity))
        };
        // A merge never swallows a higher urgency, nor a bubble the first one never got (do-not-
        // disturb, the bucket) while this one would.
        if n.replaces_id == 0
            && target.is_none()
            && let Some(i) = self.slots.iter().position(|s| {
                s.entry.identity == identity
                    && s.entry.summary == n.summary
                    && s.entry.body == n.body
                    && s.entry.urgency == n.urgency
                    && now.saturating_duration_since(s.arrived) < MERGE_WINDOW
            })
            && (self.slots[i].entry.live || !self.would_show(&identity, critical, now))
        {
            self.slots[i].arrived = now;
            return self.slots[i].entry.id;
        }
        // A live bubble replaced stays live under do-not-disturb (updated in place, its expiry
        // recomputed); it only shows once do-not-disturb is off, unless critical.
        let had_bubble = target.is_some_and(|i| self.slots[i].entry.live);
        let bubble = (had_bubble || !self.dnd || critical)
            && (had_bubble || self.live_count() < MAX_LIVE)
            && (had_bubble || critical || self.take_token(&identity, now));
        let (id, revision) = match target.and_then(|i| self.slots.remove(i)) {
            Some(old) => (old.entry.id, old.entry.revision.wrapping_add(1)),
            None if n.replaces_id != 0 => {
                self.reserve(n.replaces_id);
                (n.replaces_id, 1)
            }
            None => (self.fresh_id(), 1),
        };
        let glyph = glyph_for(&identity, n.category.as_deref());
        let expiry = if bubble { timeout(n.urgency, n.expire_ms, &self.timeouts).and_then(|d| now.checked_add(d)) } else { None };
        let kept = !n.transient && self.history > 0;
        let app = if n.app_name.is_empty() { identity.clone() } else { n.app_name };
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
            at_minute: minute,
            live: bubble,
            read: false,
            resident: n.resident,
        };
        self.slots.push_front(Slot { entry, tag: n.tag, arrived: now, expiry, kept });
        self.trim();
        id
    }

    /// Ends `id`'s bubble: `Closed(id, reason)` when it was live. `Dismissed` marks it read;
    /// `Closed` (the app's call) removes it from the history too; a transient one goes.
    pub fn close(&mut self, id: u32, reason: CloseReason) -> Vec<Effect> {
        let Some(i) = self.position(id) else { return Vec::new() };
        let s = &mut self.slots[i];
        let was_live = std::mem::replace(&mut s.entry.live, false);
        s.expiry = None;
        if reason == CloseReason::Dismissed {
            s.entry.read = true;
        }
        if reason == CloseReason::Closed || !s.kept {
            self.slots.remove(i);
        }
        if was_live { vec![Effect::Closed(id, reason)] } else { Vec::new() }
    }

    /// Runs `key` on `id`: only live, at its current `revision`, with a key it offered (a stale
    /// revision or anything else: nothing). A resident entry stays (read); any other is removed and
    /// closed as dismissed.
    pub fn invoke(&mut self, id: u32, revision: u32, key: &str) -> Vec<Effect> {
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
        out
    }

    /// The earliest expiry over the live notifications; None at rest or with sticky ones only.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.slots.iter().filter_map(|s| s.expiry).min()
    }

    /// The bubbles to draw: live, newest first, at most `max_visible`; under do-not-disturb, critical only.
    pub fn bubbles(&self) -> Vec<Shown<'_>> {
        self.slots
            .iter()
            .filter(|s| s.entry.live && (!self.dnd || s.entry.urgency == Urgency::Critical))
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
        NotifyView { unread: u16::try_from(unread).unwrap_or(u16::MAX), dnd: self.dnd, serving }
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            strip_markup("<x \"unclosed quote> then <b>b</b>"),
            "<x \"unclosed quote> then <b>b</b>",
            "unclosed to the end: all text"
        );
        for s in ["", "<", ">", "&", "<<<", "</", "<!"] {
            assert_eq!(strip_markup(s), s, "{s:?}");
        }
        for hostile in ["<".repeat(100_000), "<a".repeat(50_000), "<a \"".repeat(25_000), "&amp;".repeat(20_000), "<b>".repeat(33_000)] {
            let body = body_text(&hostile);
            assert!(body.len() <= MAX_BODY, "{} bytes from {}", body.len(), &hostile[..8]);
        }
    }

    /// Review focus: a body of only markup strips to empty; the notification still shows.
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

    // ---- timeouts (spec § 2, Mae's ruling) ----

    #[test]
    fn the_timeouts_table() {
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

    /// Review focus: the counter wraps past u32::MAX to 1, never 0, never an id in use.
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
        assert_eq!((e.revision, e.summary.as_str(), e.live, e.read, e.at_minute), (2, "Downloading 90%", true, false, 607));
        assert_eq!(history(&c), ["Downloading 90%", "Mail"], "one entry, now the newest");
        assert_eq!(shown(&c), [id], "shown again");
    }

    #[test]
    fn a_late_click_on_a_replaced_bubble_does_nothing() {
        let (mut c, t0) = (center(), Instant::now());
        let id = c.notify(with_actions(note("Equibop", "Lina: hi"), &["default"]), t0, 0);
        c.notify(Incoming { replaces_id: id, ..with_actions(note("Equibop", "Lina: hi again"), &["default"]) }, t0, 0);
        assert!(c.invoke(id, 1, "default").is_empty(), "revision 1 was replaced: stale");
        assert!(c.invoke(id, 2, "nope").is_empty(), "a key it never offered");
        assert_eq!(c.invoke(id, 2, "default"), [Effect::Invoked(id, "default".into()), Effect::Closed(id, CloseReason::Dismissed)]);
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
            c.close(id, CloseReason::Dismissed);
        }
        let newcomer = c.notify(note("newcomer", "hi"), t0, 0);
        assert!(!c.entry(newcomer).unwrap().live, "no bucket left for a new identity");
        assert_eq!(c.notify(note("newcomer", "hi"), t0 + ms(100), 0), newcomer, "still none: merged");
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

    /// Review focus: an application sending 1000 notifications in a loop.
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
        assert_eq!(c.close(dismissed, CloseReason::Dismissed), [Effect::Closed(dismissed, CloseReason::Dismissed)]);
        assert_eq!(c.entry(dismissed).map(|e| (e.live, e.read)), Some((false, true)), "kept, read");
        assert_eq!(c.close(called, CloseReason::Closed), [Effect::Closed(called, CloseReason::Closed)]);
        assert!(c.entry(called).is_none(), "the app's CloseNotification removes it");
        assert_eq!(c.expire(t0 + secs(5)), [Effect::Closed(expired, CloseReason::Expired)]);
        assert_eq!(c.entry(expired).map(|e| (e.live, e.read)), Some((false, false)), "expired: in the history, unread");
        assert!(c.close(expired, CloseReason::Closed).is_empty(), "not live: no signal");
        assert!(c.entry(expired).is_none(), "but removed");
        assert!(c.close(424_242, CloseReason::Closed).is_empty(), "unknown: nothing");
        assert_eq!(c.history().count(), 1);
    }

    #[test]
    fn an_action_closes_its_notification_unless_resident() {
        let (mut c, t0) = (center(), Instant::now());
        let id = c.notify(with_actions(note("Firefox", "Download finished"), &["default", "open"]), t0, 0);
        assert_eq!(c.invoke(id, 1, "open"), [Effect::Invoked(id, "open".into()), Effect::Closed(id, CloseReason::Dismissed)]);
        assert!(c.entry(id).is_none(), "done: out of the history too");
        let r = c.notify(Incoming { resident: true, ..with_actions(note("Rhythmbox", "Now playing"), &["next"]) }, t0, 0);
        assert_eq!(c.invoke(r, 1, "next"), [Effect::Invoked(r, "next".into())]);
        assert_eq!(c.entry(r).map(|e| (e.live, e.read)), Some((true, true)), "resident: stays");
        c.expire(t0 + secs(5));
        assert!(c.invoke(r, 1, "next").is_empty(), "no longer live: nothing runs");
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
        c.close(ids[3], CloseReason::Dismissed);
        assert_eq!(shown(&c), [ids[4], ids[2], ids[1], ids[0]], "the waiting one shows as room frees");
        let urgent = c.notify(critical("f", "f"), t0, 0);
        let top = c.bubbles()[0];
        assert_eq!((top.entry.id, top.critical), (urgent, true));
        assert!(!c.bubbles()[1].critical);
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
        for g in [EMAIL, CHAT, NETWORK, DEVICE, DOWNLOAD] {
            assert_eq!(text_width(&g.to_string()), 1, "{g:?}");
        }
    }
}
