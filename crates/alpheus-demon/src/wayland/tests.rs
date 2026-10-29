use super::*;

/// Task 4 review: the opaque region is sent only when it changes; a region that cannot be made
/// clears the surface's claim (never a stale one over transparent gaps) and is retried later.
#[test]
fn a_failed_opaque_region_clears_the_claim_and_is_retried() {
    let rect = |x: i32| Rect { x, y: 0, width: 10, height: 22 };
    let (mut sent, mut wanted) = (vec![rect(0)], vec![rect(5)]);
    let (mut set, mut cleared) = (0, 0);
    let r = send_opaque(&mut sent, &mut wanted, |_| Err("no region"), || cleared += 1);
    assert_eq!(r, Err("no region"), "the error is handed back for the log");
    assert_eq!(cleared, 1, "the stale claim is withdrawn");
    assert!(sent.is_empty(), "nothing recorded as sent");
    let r = send_opaque(
        &mut sent,
        &mut wanted,
        |rs: &[Rect]| {
            assert_eq!(rs, &[rect(5)]);
            set += 1;
            Ok::<(), &str>(())
        },
        || cleared += 1,
    );
    assert_eq!((r, set, cleared), (Ok(()), 1, 1), "the next frame retries and sends it");
    assert_eq!(sent, vec![rect(5)]);
    wanted = vec![rect(5)];
    let r = send_opaque(&mut sent, &mut wanted, |_| -> Result<(), &str> { panic!("unchanged: not sent again") }, || panic!("unchanged"));
    assert_eq!(r, Ok(()));
}

/// Task 5 review: a region that keeps failing is retried on every frame but logged once, until
/// it succeeds again.
#[test]
fn a_failing_opaque_region_is_logged_once_per_outage() {
    let mut failing = false;
    assert!(first_failure(&mut failing, true), "the first failure is logged");
    assert!(!first_failure(&mut failing, true), "the retries are not");
    assert!(!first_failure(&mut failing, true));
    assert!(!first_failure(&mut failing, false), "a success logs nothing");
    assert!(first_failure(&mut failing, true), "a new outage is logged again");
}

/// Spec § 4: a bar re-placed for a new style waits for the compositor's configure before it
/// paints again (its buffer must match the new size).
#[test]
fn a_bar_waiting_for_its_new_placement_is_not_drawn() {
    assert!(drawable(true, 800, false));
    assert!(!drawable(true, 800, true), "re-placed: the configure paints");
    assert!(!drawable(false, 800, false), "never configured");
    assert!(!drawable(true, 0, false), "no width yet");
}

/// Final review R2: the press remembered for an Activate answer is spent by the first answer
/// that uses it; a second (late) answer, or one for another icon, finds nothing.
#[test]
fn the_remembered_press_is_consumed_by_the_menu_it_opens() {
    let key = |n: u32| ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
    let press = |n: u32| Some((key(n), PopupAnchor { output: "DP-1".into(), start: 3, end: 5 }, Button::Left));
    let mut last = press(1);
    assert!(take_icon_for(&mut last, &key(1)).is_some(), "the first answer opens the menu");
    assert!(last.is_none(), "and spends the press");
    assert!(take_icon_for(&mut last, &key(1)).is_none(), "a late second answer opens nothing");
    let mut last = press(1);
    assert!(take_icon_for(&mut last, &key(2)).is_none(), "another icon's answer opens nothing");
    assert!(last.is_none(), "and the stale press is gone too");
}

#[test]
fn a_launcher_button_click_toggles_unless_this_press_just_closed_it() {
    assert_eq!(decide_click(true, false), ClickDecision::ToggleLauncher);
    assert_eq!(decide_click(true, true), ClickDecision::Nothing, "the same press that closed it as a click outside must not reopen it");
    assert_eq!(decide_click(false, true), ClickDecision::RunBarAction, "elsewhere on the bar: its own action");
    assert_eq!(decide_click(false, false), ClickDecision::RunBarAction);
}

/// Mae (2026-10-06): a click with the button that opened the menu, or a right click whatever
/// opened it, on the icon whose menu is shown closes that menu and does nothing else.
#[test]
fn closes_only_with_the_opening_button_or_a_right_click() {
    let key = |n: u32| ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
    let (a, b) = (key(1), key(2));
    // Opened by a right click (the common case): a right click closes it; a left click does
    // not — it is left to run as its own gesture (Activate), after the popup still closes as
    // a plain click outside.
    assert!(closes_own_menu(Some(&a), Some(&a), Some(Button::Right), Some(Button::Right)), "right closes its own menu");
    assert!(!closes_own_menu(Some(&a), Some(&a), Some(Button::Left), Some(Button::Right)), "left: not the opening button");
    // Opened by a left click (Steam: ItemIsMenu, or a refused Activate): the SAME left click
    // closes it too — no new Activate, no flicker.
    assert!(closes_own_menu(Some(&a), Some(&a), Some(Button::Left), Some(Button::Left)), "left closes a menu it opened");
    // A right click always closes it, whatever opened it.
    assert!(closes_own_menu(Some(&a), Some(&a), Some(Button::Right), Some(Button::Left)), "right always closes it");
    assert!(!closes_own_menu(Some(&a), Some(&b), Some(Button::Right), Some(Button::Right)), "another icon: its menu opens");
    assert!(!closes_own_menu(None, Some(&a), Some(Button::Right), Some(Button::Right)), "no menu shown: it opens");
    assert!(!closes_own_menu(Some(&a), None, Some(Button::Right), Some(Button::Right)), "elsewhere on the bar: the bar's own action");
}

/// Fix round 2026-10-06, requirement 3: the compositor may close the popup (`done`) before the
/// press that follows it is delivered — a record of what just closed covers that race.
#[test]
fn a_press_right_after_a_race_closed_popup_only_closes_too() {
    let key = |n: u32| ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
    let (a, b) = (key(1), key(2));
    let t0 = Instant::now();
    let record = (a.clone(), Button::Right, t0);
    assert!(closed_recently(Some(&record), Some(&a), Some(Button::Right), t0), "right: the closing rule's button");
    assert!(closed_recently(Some(&record), Some(&a), Some(Button::Right), t0 + Duration::from_millis(100)), "still within the window");
    assert!(!closed_recently(Some(&record), Some(&a), Some(Button::Right), t0 + RECENT_CLOSE_WINDOW), "window elapsed");
    assert!(!closed_recently(Some(&record), Some(&b), Some(Button::Right), t0), "a different icon: its own press");
    assert!(!closed_recently(Some(&record), Some(&a), Some(Button::Left), t0), "left, but it was opened and closed by right");
    assert!(!closed_recently(None, Some(&a), Some(Button::Right), t0), "no record: nothing to race against");
    let left_opened = (a.clone(), Button::Left, t0);
    assert!(closed_recently(Some(&left_opened), Some(&a), Some(Button::Left), t0), "the same button that opened it");
}

#[test]
fn a_press_on_a_module_right_after_its_popup_was_done_does_not_reopen_it() {
    use crate::popup::ModulePopup::Calendar;
    let t0 = Instant::now();
    let mut rec = Some((Calendar, t0));
    assert!(closed_by_done(&mut rec, Some(Calendar), t0 + Duration::from_millis(50)), "within the window: only closes");
    assert!(rec.is_none(), "consumed");
    let mut rec = Some((Calendar, t0));
    assert!(!closed_by_done(&mut rec, Some(Calendar), t0 + Duration::from_millis(400)), "400 ms later: reopens");
    let mut rec = Some((Calendar, t0));
    assert!(!closed_by_done(&mut rec, None, t0), "another press (no module popup) clears it");
    assert!(rec.is_none());
    assert!(!closed_by_done(&mut rec, Some(Calendar), t0), "the cleared record no longer swallows a press");
    assert!(!closed_by_done(&mut None, Some(Calendar), t0), "no record: opens");
    let mut rec = Some((Calendar, t0));
    assert!(!closed_by_done(&mut rec, Some(Calendar), t0 + RECENT_CLOSE_WINDOW), "the window's very end: elapsed, reopens");
    assert!(
        !closed_by_done(&mut Some((Calendar, t0)), Some(crate::popup::ModulePopup::Media), t0),
        "a calendar record does not swallow a media press"
    );
    assert!(
        !closed_by_done(&mut Some((Calendar, t0)), Some(crate::popup::ModulePopup::Panel), t0),
        "a calendar record does not swallow a bell press"
    );
}

/// Fix round 2026-10-06, requirement 1: a submenu fetch grafts only while the menu's current
/// selection is still the one it was asked for.
#[test]
fn a_submenu_fetch_grafts_only_while_still_selected() {
    assert!(submenu_still_selected(Some(3), Some(3)), "still on it: graft it in");
    assert!(!submenu_still_selected(Some(3), Some(7)), "selection moved elsewhere: drop it");
    assert!(!submenu_still_selected(Some(3), None), "nothing selected any more: drop it");
    assert!(!submenu_still_selected(None, Some(3)), "nothing was ever asked for");
}

/// Review requirement 3: a duplicate failed Activate (two quick left clicks on an item without
/// its own Activate, e.g. Steam) must not close and reopen the menu already asked for it.
#[test]
fn open_menu_for_the_same_key_already_asked_is_a_no_op() {
    let key = |n: u32| ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
    let (a, b) = (key(1), key(2));
    assert!(already_requested(Some(&a), &a), "the same icon: a duplicate failed Activate changes nothing");
    assert!(!already_requested(Some(&a), &b), "a different icon: its own menu still opens");
    assert!(!already_requested(None, &a), "nothing asked yet: opens normally");
}

#[test]
fn buffer_size_accounts_for_scale() {
    assert_eq!(buffer_size(100, 22, 1), 100 * 22 * 4);
    assert_eq!(buffer_size(100, 22, 2), 200 * 44 * 4);
    assert_eq!(buffer_size(100, 22, 0), 100 * 22 * 4, "scale 0 treated as 1");
}

#[test]
fn pool_already_at_floor_without_surfaces_nothing_to_do() {
    assert_eq!(target_pool(POOL_FLOOR, &[]), None);
    assert_eq!(target_pool(POOL_FLOOR / 2, &[]), None, "already under the floor: no needless recreation");
}

#[test]
fn no_more_surfaces_returns_to_floor() {
    // A large screen (3840 logical px, scale 2) grew the pool well past
    // the floor; once unplugged, no surface remains: the pool must
    // return to the floor, not stay at its peak size forever.
    assert_eq!(target_pool(4 * 1024 * 1024, &[]), Some(POOL_FLOOR));
}

#[test]
fn slight_oversize_leaves_the_pool_as_is() {
    // A pool that just doubled to fit a screen must not be recreated at
    // the slightest reduction (churn): only a clear gap
    // (POOL_OVERSIZE_FACTOR) triggers a recreation.
    let need = 676_480; // ~3840×44×4 (wide screen, scale 2)
    assert_eq!(target_pool(need * 2, &[need]), None);
    assert_eq!(target_pool(need * POOL_OVERSIZE_FACTOR, &[need]), None, "right at the factor: not clear yet");
}

#[test]
fn clear_oversize_recreates_smaller() {
    let need = 676_480;
    let target = target_pool(need * POOL_OVERSIZE_FACTOR + 1, &[need]);
    assert_eq!(target, Some(need * 2), "{target:?}");
    assert!(target.unwrap() < need * POOL_OVERSIZE_FACTOR, "clearly smaller than before");
}

#[test]
fn several_surfaces_the_sum_decides_not_the_largest() {
    // The pool holds EVERY surface's buffer at once (one per screen): it
    // needs room for their SUM, not just the largest — otherwise three
    // equally large screens would never all fit their buffers at the
    // same time in a pool sized for just one.
    let (small, large) = (10_000, 676_480);
    let sum = small + large;
    assert_eq!(target_pool(sum * POOL_OVERSIZE_FACTOR, &[small, large]), None, "right at the sum's factor: not clear yet");
    let target = target_pool(sum * POOL_OVERSIZE_FACTOR + 1, &[small, large]);
    assert_eq!(target, Some(sum * 2));
    assert!(target.unwrap() >= sum, "the recreated pool must hold every buffer at once, at least");
}

/// Fix round, 2026-10-06 (reviewer findings 2/3 on task 11): a surface
/// `draw()` skipped (never sized, an allocation failure) or whose
/// buffers a pool shrink just forgot must never carry the tray's frame
/// request — the compositor would never fire its callback, blocking
/// every later re-read.
#[test]
fn tray_sync_never_picks_a_surface_with_no_attached_buffer() {
    assert!(!eligible_for_frame(true, false, false), "configured, but nothing attached (draw() skipped it, or a pool shrink forgot it)");
    assert!(!eligible_for_frame(false, true, false), "not configured yet: no commit belongs to it either");
    assert!(eligible_for_frame(true, true, false), "configured and holding an attached buffer");
}

/// Task 6 review: a bar re-placed for a new style holds its old-size buffer until its configure;
/// a frame request committed on it meanwhile would commit that stale buffer under the new
/// placement.
#[test]
fn tray_sync_never_picks_a_bar_waiting_for_its_new_placement() {
    assert!(!eligible_for_frame(true, true, true), "re-placed, its configure not come yet");
}

/// Task 6 and 7 reviews: the hold ends on any configure except one that clearly answers the
/// PREVIOUS placement (its height), so a bar can never freeze. Two quick restyles (floating ->
/// flat -> floating before any configure) get the answer to the flat one first: it keeps holding,
/// since painting then would draw a floating bar at the flat size.
#[test]
fn only_the_answer_to_the_previous_placement_keeps_holding() {
    use alpheus_commun::theme::settings::BarStyle;
    let flat = Placement::for_style(BarStyle::Flat, 22).surface_height;
    let floating = Placement::for_style(BarStyle::Floating, 22).surface_height;
    assert_ne!(flat, floating, "the check bites only if the two placements differ in height");
    // floating -> flat -> floating: the placement before the latest one is flat.
    let previous = flat;
    assert!(still_placing(true, flat, previous, 0), "the answer to the first restyle (flat) keeps holding");
    assert!(!still_placing(true, floating, previous, 0), "the answer to the latest one ends the hold");
    assert!(!still_placing(true, 0, previous, 0), "a size left to the client (0) ends the hold");
    assert!(!still_placing(false, flat, previous, 0), "not placing: nothing to end");
    // A compositor that clamps or rounds the height (25 or 27 for 26) must never freeze the bar.
    assert!(!still_placing(true, 25, previous, 0), "an unexpected height ends the hold");
    assert!(!still_placing(true, 27, previous, 0), "an unexpected height ends the hold");
}

/// Fix wave A: a compositor that refuses the resize answers every configure at the previous
/// height. The second configure after a placement ends the hold whatever its height, so the bar
/// can never stay held.
#[test]
fn the_second_configure_after_a_placement_always_ends_the_hold() {
    use alpheus_commun::theme::settings::BarStyle;
    let previous = Placement::for_style(BarStyle::Flat, 22).surface_height;
    assert!(still_placing(true, previous, previous, 0), "the first, at the previous height: holds");
    for answered in [1, 2, u8::MAX] {
        assert!(!still_placing(true, previous, previous, answered), "after {answered} configure(s): the hold ends");
    }
}

/// Review items M3/M4, final review slice 3: the same check used by
/// `new_capability`, `remove_capability` and `remove_seat` to tell our
/// seat apart from any other on a multi-seat session.
#[test]
fn tracks_seat_only_matches_the_tracked_one() {
    assert!(tracks_seat(&Some(1), &1), "the tracked seat");
    assert!(!tracks_seat(&Some(1), &2), "a different seat");
    assert!(!tracks_seat(&None, &1), "no seat tracked at all");
}

/// Task 15 fix round, CRITICAL: the shared lookup `popup_close`/`popup_output_gone`/`hover_fire`
/// (popup.rs) now use to find their bar by surface IDENTITY, never by a `name` that
/// `update_output` can change, or that two bars could briefly share — generic so this is
/// testable without a live Wayland connection, a plain integer standing in for a
/// `wl_surface::WlSurface` (see `should_restore`'s own comment, same reasoning for
/// `wl_output::WlOutput`).
#[test]
fn position_by_goes_by_the_projected_identity_never_a_shared_name() {
    // Two entries share the "name" "": only the identity (the second element) tells them apart.
    let items = [("", 10), ("", 20), ("other", 30)];
    assert_eq!(position_by(&items, |(_, id)| id, &20), Some(1));
    assert_eq!(position_by(&items, |(_, id)| id, &10), Some(0));
    assert_eq!(position_by(&items, |(_, id)| id, &99), None, "no match: already gone");
}

#[test]
fn log_text_readable_fallback_for_an_unnamed_output() {
    assert_eq!(log_text("HDMI-A-1", 7), "HDMI-A-1");
    assert_eq!(log_text("", 7), "an output with no name (id 7)");
}

#[test]
fn should_restore_if_the_output_is_still_known() {
    // wl_output::WlOutput cannot be instantiated outside a real Wayland
    // connection (see the comment on `should_restore`): plain integers
    // stand in for output ids in this test, the function being generic
    // over `PartialEq`.
    assert!(should_restore(&2, [1, 2, 3].into_iter()), "still in the list: to be restored");
    assert!(!should_restore(&5, [1, 2, 3].into_iter()), "no longer in the list: really unplugged");
    assert!(!should_restore(&1, std::iter::empty::<i32>()), "no output known at all anymore");
}

#[test]
fn plan_buffer_resizes_when_none_yet() {
    assert_eq!(plan_buffer(&[], (100, 22)), BufferPlan::Resize);
}

#[test]
fn plan_buffer_reuses_the_one_free_buffer() {
    assert_eq!(plan_buffer(&[((100, 22), false)], (100, 22)), BufferPlan::Reuse(0));
}

#[test]
fn plan_buffer_grows_when_the_only_buffer_is_busy() {
    // The whole point of double-buffering: a single buffer is always
    // busy right up to the moment a new one replaces it (see
    // `BufferPlan`'s doc) — this must grow to a second, not stall.
    assert_eq!(plan_buffer(&[((100, 22), true)], (100, 22)), BufferPlan::Grow);
}

#[test]
fn plan_buffer_reuses_whichever_of_two_is_free() {
    assert_eq!(plan_buffer(&[((100, 22), true), ((100, 22), false)], (100, 22)), BufferPlan::Reuse(1));
    assert_eq!(plan_buffer(&[((100, 22), false), ((100, 22), true)], (100, 22)), BufferPlan::Reuse(0));
}

#[test]
fn plan_buffer_grows_when_both_of_two_are_busy() {
    assert_eq!(plan_buffer(&[((100, 22), true), ((100, 22), true)], (100, 22)), BufferPlan::Grow);
}

#[test]
fn plan_buffer_resizes_on_a_size_change_whatever_the_busy_state() {
    // Wrong size: every previous buffer is useless either way, free or busy.
    assert_eq!(plan_buffer(&[((100, 22), false)], (120, 22)), BufferPlan::Resize);
    assert_eq!(plan_buffer(&[((100, 22), true)], (120, 22)), BufferPlan::Resize);
    assert_eq!(plan_buffer(&[((100, 22), false), ((100, 22), true)], (120, 22)), BufferPlan::Resize);
}

#[test]
fn should_restore_once_bounds_repeated_closures() {
    // Still known, never yet restored: to be restored.
    assert!(should_restore_once(&2, &[], [1, 2, 3].into_iter()));
    // Already restored (in `pending`), no configure received since: a
    // compositor that closed its surface in a loop must NOT be restored
    // a second time as long as the first one hasn't succeeded.
    assert!(!should_restore_once(&2, &[2], [1, 2, 3].into_iter()));
    // No longer known at all: never to be restored, even absent from `pending`.
    assert!(!should_restore_once(&5, &[], [1, 2, 3].into_iter()));
}

#[test]
fn pending_restores_keeps_only_still_known_outputs() {
    // 2 is still known at after-dispatch time: restored. 1 no longer
    // is (its `output_destroyed`, in the same batch, already ran): the
    // hot-plug race this fixes — left alone, no extra remove+place.
    assert_eq!(pending_restores(&[1, 2], [2, 3].into_iter()), vec![2]);
}

#[test]
fn pending_restores_empty_when_nothing_marked() {
    assert_eq!(pending_restores::<i32>(&[], [1, 2, 3].into_iter()), Vec::<i32>::new());
}

#[test]
fn pending_restores_empty_when_none_of_the_marked_outputs_survived() {
    // Every output marked for restore by `closed` turned out to be
    // truly gone by the time the batch finished: nothing restored.
    assert_eq!(pending_restores(&[5, 6], [1, 2, 3].into_iter()), Vec::<i32>::new());
}

#[test]
fn pending_restores_preserves_the_marked_order() {
    assert_eq!(pending_restores(&[3, 1, 2], [1, 2, 3].into_iter()), vec![3, 1, 2]);
}

/// The bar paints with its own style and module colours and the theme's opacity, over the
/// grid's blocks: what `draw` hands `paint_bar`.
#[test]
fn a_bar_paints_with_its_style_its_module_colours_and_the_theme_opacity() {
    use alpheus_commun::theme::settings::{BarStyle, ModuleColors};
    let mut bar = Bar::new(crate::config::BarConfig::default());
    bar.apply(Msg::Compositor(Some(crate::compositor::example())));
    bar.style = BarStyle::Underline;
    bar.module_colors = ModuleColors::Pills;
    let settings = ThemeSettings { opacity: 0.55, style: BarStyle::Islands, module_colors: ModuleColors::None, ..ThemeSettings::default() };
    let grid = bar.grid("HDMI-A-1", 100);
    assert!(!grid.groups().is_empty());
    let bg = bar_background(&bar, &settings, &grid);
    assert_eq!((bg.style, bg.opacity, bg.module_colors), (BarStyle::Underline, 0.55, ModuleColors::Pills), "the bar's, not the file's");
    assert_eq!(bg.groups, grid.groups());
}
