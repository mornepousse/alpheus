# The calendar

A month grid under the clock. This page says what it does, how to use it, how to configure it, how
it is built, which rules the tests enforce on it, and what it costs. It describes the code as it is
today.

## What it does

A left click on the clock opens a popup under it with the current month as a grid of cells:

- the title (`October 2026`) centred in bold accent colour, then the weekday initials (`Mo Tu We`)
  dim, then the days;
- today inverted, like the active workspace;
- the first day of the week taken from the locale, Monday when the locale says nothing usable;
- optionally the ISO 8601 week number of each row, dim, on the left;
- only the weeks of the shown month, 4 to 6 rows (the popup's height follows them).

The text is English, like the clock's. There are no events and no network, and nothing is kept
while the calendar is closed: a `Calendar` is built at each open and dropped at close.

## Using it

- **Left click on the clock** opens the calendar; a second left click on the clock closes it.
- **← and →** show the previous and the next month. The **mouse wheel** over the popup does the
  same.
- **Esc**, or a click outside the popup, closes it.

The popup is opened with a grab (`Input::Grab`). The README notes what that means for the bar's
keyboard on Hyprland.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar).

| Key (under `[clock]`) | Default | Meaning |
| --- | --- | --- |
| `calendar` | `true` | a left click on the clock opens the calendar; `false` opens nothing |
| `week_numbers` | `false` | show the ISO week numbers on the left of the days |

The first day of the week is not configured: it comes from the locale's `LC_TIME` (`LC_ALL`,
`LC_TIME`, `LANG`, in glibc's order).

```toml
[clock]
week_numbers = true
```

## How it is built

Two files in `crates/alpheus-daemon/src/calendar/`:

- `mod.rs`: the pure model. `Calendar` (the shown month, today, the first day, whether week numbers
  show), `Month`, `iso_week`, and `locale_first_day` (the one function that reads the locale, with
  three small `unsafe` blocks that each say why they are sound). `Calendar::render` returns the
  popup's rows as cell grids. Dates are worked out in the file itself (days counted from 1 January
  1970), with no date crate. The tests sit at the bottom of the file, with their snapshots in
  `calendar/snapshots/`.
- `app.rs`: the wiring. `OPS` is the calendar's row in the table of popups (`popup::POPUPS`):
  the toggle, the key handler and the pointer handler. `calendar_toggle` builds the `Calendar` from
  the current local time, the locale's first day and `[clock] week_numbers`, opens the popup and
  paints it; `calendar_scroll` moves the month and repaints.

**The path of a click.** The bar's click handler (`App::click` in `wayland.rs`) finds the clock's
zone and calls the popup table's `toggle` for it, which is `calendar_toggle`. Inside the popup a key
reaches `calendar_key` and a wheel step `calendar_pointer`; both end in `calendar_scroll`, or in
`popup_close` for Esc.

## Rules it keeps

- `the_tray_spawns_no_thread`: `calendar/mod.rs` starts no thread, process or command.
- `render_normalises_out_of_range_fields`: `shown` and `first_day` are public fields, and `render`
  never panics whatever they hold.
- The headless tests `the_calendar_opens_on_the_clock_and_closes_on_escape`,
  `calendar_false_opens_nothing`, `an_open_calendar_is_repainted_too`,
  `an_output_going_away_closes_the_calendar`, `opening_the_launcher_closes_the_calendar` and
  `a_tray_menu_replaces_the_calendar` pin how it opens and closes.
- The reasons are in [decisions.md](../decisions.md): "The calendar is written in English, like the
  clock", "The calendar's first day comes from the locale, read at each open", "Calendar week
  numbers are ISO 8601".

## What it costs

The calendar has no figure of its own in [measurements](../measurements.md): it was measured with the tray's menus.

- **Closed:** nothing exists of it. Without a click on the clock nothing is built, and the locale
  is not read at start.
- **Open and closed twenty times:** the tray's memory test cycles a menu and the calendar together
  and reads +32 KB in each of 3 runs (`popups_free_everything_on_close`).
