# House style

> Draft from the timer pilot; amended after review.

This page says how alpheus's code is written, so that someone who reads Rust with difficulty can
still find a feature, read it and change it. Every rule below is shown with a real pair of
"before" and "after" taken from the commit that brought the focus timer to this standard
(`docs/features/timer.md` describes the feature; its code is in `crates/alpheus-daemon/src/timer/`).
Where the timer already followed a rule and nothing had to change, the example is a line that
shows it.

The test for every rule is one question: can a programmer who knows another language, with no AI
assistant, find this, read it and change it?

## 1. A module says what it is, what it never does, and where to read more

Every `.rs` file starts with a `//!` header in four parts, in this order (test files are the
exception: a short `//!` saying what the file tests is enough):

1. one plain sentence saying what the file is;
2. a short paragraph of what it holds and how it fits with its neighbours, if that is needed;
3. `Never:` lines, one per rule the file keeps (reads no clock, starts nothing, never panics);
4. a last `See:` line pointing to the spec, the feature page and the entries of `docs/decisions.md`.

Before:

```rust
//! The focus timer's model (timer spec): a countdown, a Pomodoro or a stopwatch, every change
//! made to it, the end it comes to, the chip it draws, when the chip next changes, the state
//! file's line and the notification an end becomes. Pure: times are wall-clock Unix milliseconds
//! handed in by the wiring (`timer_app.rs`), nothing here reads a clock, a file or starts anything.
```

After:

```rust
//! The focus timer's model: a countdown, a Pomodoro or a stopwatch, and everything it shows.
//!
//! This is the pure half of the timer. It holds the timer's state (`Timer`), every change that can
//! be made to it (`Change`), ... The other half, `app.rs`, does the talking to the outside: it
//! reads the clock, the keys and the file, and hands this file every time it needs as Unix
//! milliseconds (wall-clock time counted from 1 January 1970).
//!
//! Never:
//! - read a clock: the caller always passes `now`, so a test can pick any time it likes;
//! - read or write a file, or start anything (a thread, a process, a timer).
//!
//! See: docs/specs/2026-11-15-alpheus-timer-design.md, docs/features/timer.md, and
//! docs/decisions.md ("The timer counts wall-clock time", "A late end is applied once").
```

Every fact of the old header is still there; the new one adds what an unfamiliar word means
(`Unix milliseconds`) and which file does the rest.

A word of warning: most guardrail tests read a file's text, comments included, for forbidden
words (`thread::`, `spawn(`, `.unwrap()`). Do not write those exact strings in a comment of a file
they cover; say it in words ("starts a thread"). The ones that read code to find what calls what
or which function owns a line (`the_timer_is_wired_through_one_path`,
`the_timer_popup_closing_drops_its_tick_and_sees_the_end`, `every_dnd_change_goes_through_dnd_apply`)
strip comments first (`without_comments` in `tests/guardrails.rs`), so a comment there is harmless.

## 2. Every non-obvious line says why, in a full sentence

A comment says why, not what the next line does. Constants, early returns and odd-looking
numbers get a sentence.

Before:

```rust
/// Two deadlines this close are the same one.
const SAME_DEADLINE: Duration = Duration::from_millis(5);
```

After:

```rust
/// Two deadlines this close are the same one: `timer_sync` leaves an armed deadline alone rather
/// than re-arm it for a few milliseconds' difference, so the clock tick does not re-arm a timer
/// already set.
const SAME_DEADLINE: Duration = Duration::from_millis(5);
```

Before:

```rust
Self::Countdown { run: Run::Running(end), total } => {
    Self::Countdown { run: Run::Paused(end.saturating_sub(now).min(total)), total }
}
```

After (same code, one comment above it):

```rust
// The time left is cut to the total: a wall clock set backwards would otherwise make
// it look longer than the countdown itself.
```

When the reason is a settled decision, the comment points to its entry in `docs/decisions.md`
instead of citing a person or a plan ("ruling: ...", "plan decision 17"):

Before:

```rust
/// An end shown in the popup is seen by the time it closes (ruling: the chip does not say `done`
/// about something the popup already said).
```

After:

```rust
/// The change to make when the popup closes: `Seen` if the timer reads `done`, else none.
///
/// An end the popup showed is seen by the time it closes: the chip must not say `done` about
/// something the popup already said. See docs/decisions.md, "`done` is cleared by looking, and is
/// never saved".
```

## 3. Plain forms over clever ones, when both compile to the same thing

Early returns, `match` and `let ... else` are preferred over long chains of `Option` or `Result`
methods (`and_then`, `map_or`, `then_some`, `filter`). A chain is fine when it is short and its
meaning is obvious; when a reader has to stop and unwind it, write the `match`.

Before:

```rust
let at = self.timer.model.next_change(now, self.timer_shown()).and_then(|t| Instant::now().checked_add(wait_for(t, now)));
```

After:

```rust
// The next change is a Unix time; it becomes an `Instant` (the event loop's own clock) by
// adding the wait to the current instant. `checked_add` gives None instead of overflowing.
let at = match self.timer.model.next_change(now, self.timer_shown()) {
    Some(t) => Instant::now().checked_add(wait_for(t, now)),
    None => None,
};
```

Before:

```rust
let round = u8::try_from(round).ok().filter(|r| (1..=ROUNDS_MAX).contains(r))?;
```

After:

```rust
let Ok(round) = u8::try_from(round) else { return None };
if !(1..=ROUNDS_MAX).contains(&round) {
    return None;
}
```

A closure that is used to name a step becomes a named function, with a doc comment.

Before:

```rust
let ahead = |end: u64| (end <= now.saturating_add(SAVED_AHEAD_MS)).then_some(end);
```

After:

```rust
/// A running end read from the state file, if it is believable: at most `SAVED_AHEAD_MS` after
/// `now`. An end in the past is fine (the end passed while the daemon was off, and is applied once
/// at start); one far in the future cannot be ours.
fn believable_end(end: u64, now: u64) -> Option<u64> {
    if end <= now.saturating_add(SAVED_AHEAD_MS) { Some(end) } else { None }
}
```

The same rule removes a repeated check. The chip and the popup both asked "is this timer paused?"
with the same five-line `matches!`; it is now one method, `Timer::paused_now`, which both call.

## 4. Where a Rust form stays, explain it once, where it is used

Some forms cannot be made plainer without losing something, and a newcomer will stumble on them.
The first time one appears in a file, a short comment says what it means.

```rust
// The text split on spaces. Each pattern below matches a list of exactly that many fields,
// the first ones being fixed words; anything of another shape reaches the last arm, `_`.
// `?` after a call that gives None ends the function with None: the text is refused.
let fields: Vec<&str> = text.split_whitespace().collect();
match fields.as_slice() {
    ["countdown", state, end_or_left, total] => {
```

```rust
// Taking the two fields out of `self` at once lets the borrow checker see that the popup
// and the timer are different things, so one can change while the other is read.
let App { popup, timer, .. } = self;
```

Other forms worth a comment where they appear: `pub(crate)` (visible inside the daemon only),
`saturating_sub` and `saturating_mul` (stop at the limit instead of wrapping round), `{x:02}` in a
format string (pad to two digits), a closure that captures nothing used where a function is
expected.

## 5. Names are long, plain and descriptive

No abbreviations beyond the project's glossary. A helper is named for what it means.

Before (test helpers, `timer/tests.rs`, `timer/popup/tests.rs`):

```rust
fn pomo(end: u64, phase: Phase, round: u8) -> Timer { ... }
fn chip(t: Timer, now: u64) -> Option<(char, Option<&'static str>, ChipLook)> { ... }
fn every_state() -> Vec<Timer> { ... }
```

After:

```rust
/// A Pomodoro phase that is running and ends at `end`.
fn running_pomodoro(end: u64, phase: Phase, round: u8) -> Timer { ... }
/// What the chip shows at `now` (idle chip as a glyph): its glyph, text and look.
fn chip_parts(t: Timer, now: u64) -> Option<(char, Option<&'static str>, ChipLook)> { ... }
/// One timer of every kind, running and paused, for the state file's round trip.
fn a_timer_in_every_state() -> Vec<Timer> { ... }
```

## 6. Tests: names are sentences of behaviour, one behaviour each

A test's name says what is true, so that a failure reads as a sentence. Each test file starts
with a `//!` line saying what it covers. Tests use fakes (`FakeExecutor`), never a real session
tool.

Before and after (`timer/tests.rs`):

```rust
fn start_pause_resume_add_stop()
fn a_countdown_starts_pauses_resumes_takes_a_minute_and_stops()

fn left_is_clamped_and_words_read()
fn the_time_left_is_clamped_and_each_kind_has_its_word()

fn texts()
fn minutes_clock_and_length_texts_read_as_written()
```

```rust
//! Tests of the timer's model (`mod.rs`): every change, the ends, the chip, the next deadline, the
//! state file's line and the notification texts. No clock, no file, no daemon: each test passes
//! the time it wants.
```

Test names that other documents cite (memory and headless tests are cited with `--exact`) are
never renamed in a tidy-up commit. This covers tests cited outside docs/specs/ and docs/plans/.

## 7. Rules the timer already followed

The timer already followed these, except the last one; the lines below show them.

- **Closed sets are enums, not traits.** `Change` (what can be asked), `End` (what can end) and
  `Phase` are enums, so every place that handles them is a `match` the compiler checks is
  complete.
- **No `unwrap`, `expect` or `panic!` outside a documented invariant or a test.** For the timer a
  guardrail test (`the_timer_has_no_panicking_path`) fails the build if one appears.
- **No new macro or generic abstraction without a reason written in the commit or the pull
  request.** The timer has none.
- **Every `unsafe` block says why it is sound.** The timer has none.
- **Early returns.** Functions such as `Timer::chip` return at once for the idle cases and keep
  the main path unindented. This one was introduced by the rewrite, not already there.

## 8. The decisions file

`docs/decisions.md` holds settled decisions, newest last. An entry has this shape:

```
### <the decision, as a short sentence>

One or two sentences: what is decided, and why.

Date: <YYYY-MM-DD> · Source: <the spec section, plan decision or commit it comes from>
```

Wording is neutral ("decision", never a person's name). When code depends on a decision that would
look arbitrary on its own, the comment names the entry by its title, and the entry names its source.
