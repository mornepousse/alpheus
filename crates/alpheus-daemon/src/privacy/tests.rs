//! Tests of the privacy model (`mod.rs`): the rules that turn the PipeWire graph into who holds the
//! microphone, the camera and the screen, the merge with the compositor and the video devices, the
//! off-delay, and the popup's rows. No file and no daemon; the time is passed in by each test, with
//! one `Instant::now` per test as the start.

use super::*;

/// A node of this class that is running.
fn running_node(class: &str) -> GraphNode {
    GraphNode { class: class.into(), running: true, ..GraphNode::default() }
}

/// A running node of this class that belongs to the app `name`.
fn app_node(class: &str, name: &str) -> GraphNode {
    GraphNode { app: Some(name.into()), ..running_node(class) }
}

/// A link from the node `output` to the node `input`.
fn graph_link(output: u32, input: u32, active: bool) -> GraphLink {
    GraphLink { output, input, active }
}

/// What the rules find in a graph of these nodes (by id) and links.
fn capture_of(nodes: Vec<(u32, GraphNode)>, links: Vec<GraphLink>) -> Capture {
    let nodes = nodes.into_iter().collect();
    let links = links.into_iter().enumerate().map(|(i, l)| (i as u32 + 100, l)).collect();
    capture(&nodes, &links)
}

/// An indicator held by these apps, and by an unnamed holder when `unnamed`.
fn indicator(names: &[&str], unnamed: bool) -> Indicator {
    Indicator { names: names.iter().map(|s| s.to_string()).collect(), unnamed }
}

#[test]
fn the_microphone_is_a_source_linked_to_a_recording_stream() {
    for source in ["Audio/Source", "Audio/Source/Virtual"] {
        let c = capture_of(vec![(1, running_node(source)), (2, app_node("Stream/Input/Audio", "firefox"))], vec![graph_link(1, 2, true)]);
        assert_eq!(c.mic, indicator(&["firefox"], false), "{source}");
        assert!(!c.camera.active() && !c.screen.active());
    }
}

#[test]
fn meters_visualisers_filters_and_passive_streams_never_light_it() {
    let flags: [fn(&mut GraphNode); 4] = [|n| n.monitor = true, |n| n.capture_sink = true, |n| n.link_group = true, |n| n.passive = true];
    for set in flags {
        let mut consumer = app_node("Stream/Input/Audio", "pavucontrol");
        set(&mut consumer);
        let c = capture_of(vec![(1, running_node("Audio/Source")), (2, consumer)], vec![graph_link(1, 2, true)]);
        assert!(!c.mic.active());
    }
    let c = capture_of(vec![(1, running_node("Audio/Source")), (2, app_node("Stream/Input/Audio/Foo", "x"))], vec![graph_link(1, 2, true)]);
    assert!(!c.mic.active());
}

#[test]
fn an_idle_link_lights_it_only_while_the_stream_runs() {
    let idle = |running| GraphNode { running, ..app_node("Stream/Input/Audio", "firefox") };
    let lit = |running, active| {
        capture_of(vec![(1, running_node("Audio/Source")), (2, idle(running))], vec![graph_link(1, 2, active)]).mic.active()
    };
    assert!(!lit(false, false));
    assert!(lit(true, false));
    assert!(lit(false, true));
}

#[test]
fn an_app_muted_stream_still_lights_the_microphone() {
    // The model has no per-stream mute: a running, linked stream holds the device.
    let mut consumer = app_node("Stream/Input/Audio", "firefox");
    consumer.passive = false;
    let c = capture_of(vec![(1, running_node("Audio/Source")), (2, consumer)], vec![graph_link(1, 2, true)]);
    assert_eq!(c.mic, indicator(&["firefox"], false));
}

#[test]
fn a_camera_is_a_device_video_source_linked_to_a_video_stream() {
    let cam = GraphNode { camera_device: true, ..running_node("Video/Source") };
    let c = capture_of(vec![(1, cam), (2, app_node("Stream/Input/Video", "org.chromium.Chromium"))], vec![graph_link(1, 2, true)]);
    assert_eq!(c.camera, indicator(&["org.chromium.Chromium"], false));
    assert!(!c.screen.active());
}

#[test]
fn a_running_camera_with_no_link_lights_it_unnamed() {
    let cam = GraphNode { camera_device: true, ..running_node("Video/Source") };
    let c = capture_of(vec![(1, cam.clone())], vec![]);
    assert_eq!(c.camera, indicator(&[], true));
    let idle = GraphNode { running: false, ..cam };
    assert!(!capture_of(vec![(1, idle)], vec![]).camera.active());
}

#[test]
fn a_portal_or_niri_producer_is_the_screen() {
    let c =
        capture_of(vec![(1, running_node("Video/Source")), (2, app_node("Stream/Input/Video", "firefox"))], vec![graph_link(1, 2, true)]);
    assert_eq!(c.screen, indicator(&["firefox"], false));
    assert!(!c.camera.active());
    let c = capture_of(vec![(1, running_node("Stream/Output/Video"))], vec![]);
    assert_eq!(c.screen, indicator(&[], true));
    let idle = GraphNode { running: false, ..running_node("Video/Source") };
    let c = capture_of(vec![(1, idle.clone()), (2, running_node("Stream/Input/Video"))], vec![graph_link(1, 2, false)]);
    assert!(!c.screen.active());
    assert!(!capture_of(vec![(1, idle)], vec![]).screen.active());
}

#[test]
fn names_are_deduplicated_sorted_and_capped() {
    let mut nodes = vec![(100, running_node("Audio/Source"))];
    let mut links = vec![];
    for i in (0..10u32).rev() {
        nodes.push((i + 1, app_node("Stream/Input/Audio", &format!("app{i}"))));
        links.push(graph_link(100, i + 1, true));
    }
    nodes.push((50, app_node("Stream/Input/Audio", "app3")));
    links.push(graph_link(100, 50, true));
    let c = capture_of(nodes.clone(), links.clone());
    let expect: Vec<String> = (0..8).map(|i| format!("app{i}")).collect();
    assert_eq!(c.mic.names, expect);
    assert!(!c.mic.unnamed);
    nodes.push((60, running_node("Stream/Input/Audio")));
    links.push(graph_link(100, 60, true));
    let c = capture_of(nodes, links);
    assert_eq!(c.mic.names, expect);
    assert!(c.mic.unnamed);
}

#[test]
fn without_drops_ignored_names_case_insensitively() {
    let c = Capture { mic: indicator(&["Firefox", "obs"], false), ..Capture::default() };
    let ignore = ["FIREFOX".to_string()];
    assert_eq!(c.without(&ignore).mic, indicator(&["obs"], false));
    let c = Capture { mic: indicator(&["Firefox"], false), ..Capture::default() };
    let stripped = c.without(&ignore);
    assert!(!stripped.mic.active() && stripped.is_empty());
    let c = Capture { mic: indicator(&["Firefox"], true), ..Capture::default() };
    assert!(c.without(&ignore).mic.active());
}

#[test]
fn only_the_classes_the_rules_need_are_kept() {
    for yes in ["Audio/Source", "Audio/Source/Virtual", "Stream/Input/Audio", "Stream/Input/Video", "Video/Source", "Stream/Output/Video"] {
        assert!(kept(yes), "{yes}");
    }
    for no in ["Audio/Sink", "Stream/Output/Audio", "Midi/Bridge"] {
        assert!(!kept(no), "{no}");
    }
}

/// A capture in which only the microphone is held, by these apps.
fn mic_held_by(names: &[&str]) -> Capture {
    Capture { mic: indicator(names, false), ..Capture::default() }
}

/// `n` milliseconds.
fn millis(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn lighting_up_is_immediate_and_going_off_waits_the_delay() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    assert!(p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0));
    assert!(p.shown().mic.active());
    assert_eq!(p.deadline(), None);
    let t1 = t0 + Duration::from_secs(1);
    assert!(!p.set_sound(Some(&Capture::default()), false, t1), "still shown");
    assert!(p.shown().mic.active());
    assert_eq!(p.deadline(), Some(t1 + OFF_DELAY));
    assert!(!p.due(t1 + millis(400)));
    assert!(p.shown().mic.active());
    assert!(p.due(t1 + OFF_DELAY));
    assert!(p.shown().is_empty());
    assert_eq!(p.deadline(), None);
}

#[test]
fn a_later_setter_never_pushes_the_deadline_back() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0);
    p.set_sound(Some(&Capture::default()), false, t0 + millis(100));
    p.set_sound(Some(&Capture::default()), true, t0 + millis(300)); // a mute flip settles again
    p.set_casting(true, Duration::ZERO, t0 + millis(350));
    p.set_casting(false, Duration::ZERO, t0 + millis(360));
    p.set_sound(Some(&Capture::default()), false, t0 + millis(400));
    assert_eq!(p.deadline(), Some(t0 + millis(100) + OFF_DELAY));
}

#[test]
fn coming_back_within_the_delay_never_blinks() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0);
    p.set_sound(Some(&Capture::default()), false, t0 + millis(100));
    assert!(!p.shown().is_empty());
    p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0 + millis(400));
    assert!(!p.shown().is_empty());
    assert_eq!(p.deadline(), None, "the pending off is cleared");
    assert!(!p.due(t0 + Duration::from_secs(5)));
    assert!(p.shown().mic.active());
}

#[test]
fn hyprland_releases_count_after_the_hold() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    assert!(p.set_casting(true, CAST_HOLD, t0));
    assert!(p.shown().screen.unnamed && p.shown().screen.names.is_empty());
    assert_eq!(p.deadline(), None);
    let r = t0 + millis(500);
    assert!(!p.set_casting(false, CAST_HOLD, r));
    assert!(p.shown().screen.active());
    assert_eq!(p.deadline(), Some(r + CAST_HOLD + OFF_DELAY));
    assert!(!p.due(r + CAST_HOLD + millis(200)), "the hold ended 200 ms ago: the delay counts from the release");
    assert!(p.shown().screen.active());
    assert_eq!(p.deadline(), Some(r + CAST_HOLD + OFF_DELAY));
    assert!(!p.due(r + CAST_HOLD + OFF_DELAY - millis(1)));
    assert!(p.due(r + CAST_HOLD + OFF_DELAY));
    assert!(p.shown().is_empty());
    assert_eq!(p.deadline(), None);
}

#[test]
fn a_cast_coming_back_during_the_hold_never_blinks() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_casting(true, CAST_HOLD, t0);
    p.set_casting(false, CAST_HOLD, t0 + millis(100));
    p.set_casting(true, CAST_HOLD, t0 + millis(600));
    assert_eq!(p.deadline(), None);
    assert!(!p.due(t0 + Duration::from_secs(9)));
    assert!(p.shown().screen.active());
}

#[test]
fn a_zero_hold_goes_off_at_release_plus_the_delay() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_casting(true, Duration::ZERO, t0);
    p.set_casting(false, Duration::ZERO, t0 + millis(200));
    assert_eq!(p.deadline(), Some(t0 + millis(200) + OFF_DELAY));
    assert!(p.due(t0 + millis(200) + OFF_DELAY));
    assert!(p.shown().is_empty());
}

#[test]
fn the_screen_merges_pipewire_and_the_compositor() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    let named = Capture { screen: indicator(&["firefox"], false), ..Capture::default() };
    p.set_sound(Some(&named), false, t0);
    p.set_casting(true, CAST_HOLD, t0);
    assert_eq!(p.shown().screen, indicator(&["firefox"], false));
    let mut q = Privacy::new(Vec::new());
    q.set_casting(true, CAST_HOLD, t0);
    assert_eq!(q.shown().screen, indicator(&[], true));
    // PipeWire's names appear when its graph catches up with the compositor.
    q.set_sound(Some(&named), false, t0 + millis(100));
    assert_eq!(q.shown().screen, indicator(&["firefox"], false));
}

#[test]
fn ignore_applies_before_the_delay() {
    let t0 = Instant::now();
    let mut p = Privacy::new(vec!["obs".into()]);
    assert!(!p.set_sound(Some(&mic_held_by(&["obs"])), false, t0));
    assert!(p.shown().is_empty());
    assert_eq!(p.deadline(), None);
}

#[test]
fn the_microphone_dims_with_the_default_source_mute() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    assert!(!p.mic_muted());
    assert!(p.set_sound(Some(&mic_held_by(&["firefox"])), true, t0));
    assert!(p.mic_muted());
    assert!(p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0 + millis(10)), "the mute alone changes it");
    assert!(!p.mic_muted());
    assert!(!p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0 + millis(20)), "nothing changed");
}

/// One heading per active indicator in the module's order, its glyph in its
/// hue, then one row per app indented 4 cells, or `(unnamed)`; the box as wide as the longest
/// row and a blank cell on each side, at most `POPUP_WIDTH`.
#[test]
fn the_popup_lists_each_indicator_and_its_apps() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0);
    p.set_casting(true, CAST_HOLD, t0);
    let b = p.render();
    let want = [format!("{MIC} Microphone"), "    firefox".into(), format!("{SCREEN} Screen"), "    (unnamed)".into()];
    let texts: Vec<String> = b.rows.iter().map(|g| g.text().trim_end().to_string()).collect();
    assert_eq!(texts, want.iter().map(|r| format!(" {r}")).collect::<Vec<_>>());
    let longest = want.iter().map(|r| text_width(r)).max().unwrap_or(0);
    assert_eq!(b.cells(), (longest + 2, 4));
    let (mic, screen) = (b.rows[0].styles(), b.rows[2].styles());
    assert!(mic.starts_with(".o.") && screen.starts_with(".r."), "glyphs in their hues: {mic} {screen}");
    assert!(b.rows[1].styles().chars().all(|c| c == '.'), "names in the normal style");

    let long = "n".repeat(60);
    p.set_sound(Some(&mic_held_by(&[long.as_str()])), false, t0);
    assert_eq!(p.render().cells().0, POPUP_WIDTH, "capped");
    p.set_sound(Some(&mic_held_by(&["firefox"])), true, t0);
    assert!(p.render().rows[0].styles().starts_with(".m."), "a muted microphone dims, as in the bar");
}

#[test]
fn a_sound_source_that_is_down_counts_as_nothing_captured() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_sound(Some(&mic_held_by(&["firefox"])), false, t0);
    assert!(!p.set_sound(None, false, t0 + millis(100)));
    assert!(p.shown().mic.active());
    assert_eq!(p.deadline(), Some(t0 + millis(100) + OFF_DELAY));
    assert!(p.due(t0 + millis(100) + OFF_DELAY));
    assert!(p.shown().is_empty());
}

#[test]
fn a_device_holder_lights_the_camera_at_once() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    assert!(p.set_devices(indicator(&["fakecam"], false), t0));
    assert_eq!(p.shown().camera.names, ["fakecam"]);
    assert_eq!(p.deadline(), None);
}

#[test]
fn device_and_pipewire_names_merge_once() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    let pw = Capture { camera: indicator(&["recorder"], false), ..Capture::default() };
    p.set_sound(Some(&pw), false, t0);
    assert!(p.set_devices(indicator(&["fakecam", "recorder"], false), t0));
    assert_eq!(p.shown().camera.names, ["fakecam", "recorder"]);
}

#[test]
fn a_device_holder_goes_through_the_off_delay() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_devices(indicator(&["fakecam"], false), t0);
    assert!(!p.set_devices(Indicator::default(), t0));
    assert_eq!(p.deadline(), Some(t0 + OFF_DELAY));
    assert!(!p.due(t0 + millis(499)));
    assert!(p.due(t0 + OFF_DELAY));
    assert!(!p.shown().camera.active());
}

#[test]
fn ignore_drops_a_device_holder() {
    let t0 = Instant::now();
    let mut p = Privacy::new(vec!["FakeCam".into()]);
    assert!(!p.set_devices(indicator(&["fakecam"], false), t0));
    assert!(p.shown().is_empty());
}

#[test]
fn a_device_back_within_the_delay_never_blinks() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    p.set_devices(indicator(&["fakecam"], false), t0);
    p.set_devices(Indicator::default(), t0);
    p.set_devices(indicator(&["fakecam"], false), t0 + millis(200));
    assert_eq!(p.deadline(), None);
    assert!(p.shown().camera.active());
}

#[test]
fn merging_two_indicators_keeps_unnamed_and_caps_the_names() {
    assert!(indicator(&["a"], false).merged(&indicator(&[], true)).unnamed);
    assert!(indicator(&[], true).merged(&indicator(&["a"], false)).unnamed);
    let many: Vec<String> = (0..12).map(|n| format!("app{n:02}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(indicator(&many[..6], false).merged(&indicator(&many[6..], false)).names.len(), MAX_NAMES);
}

/// The popup is as wide as its widest row, and a heading counts its glyph: with one short app name
/// the heading is the widest row.
#[test]
fn the_popups_width_counts_the_glyph_of_a_heading() {
    let mut p = Privacy::new(Vec::new());
    p.set_sound(Some(&mic_held_by(&["a"])), false, Instant::now());
    let heading = text_width(&format!("{MIC} Microphone"));
    assert_eq!(p.render().cells().0, heading + 2);
}

/// While PipeWire names a holder of the screen, a compositor's release adds no deadline of its own:
/// the screen stays lit by PipeWire.
#[test]
fn a_release_adds_no_deadline_while_pipewire_holds_the_screen() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    let named = Capture { screen: indicator(&["firefox"], false), ..Capture::default() };
    p.set_sound(Some(&named), false, t0);
    p.set_casting(true, CAST_HOLD, t0);
    p.set_casting(false, CAST_HOLD, t0 + millis(100));
    assert_eq!(p.deadline(), None);
}

/// A recording alpheus runs lights the screen at once, named `alpheus` (the compositor and PipeWire
/// may not see a recorder that bypasses them), beside what PipeWire names; it goes off one delay
/// after the recording ends, as any holder does. Nothing else is lit.
#[test]
fn recording_lights_the_privacy_screen_glyph() {
    let t0 = Instant::now();
    let mut p = Privacy::new(Vec::new());
    assert!(p.set_recording(true, t0), "lit at once");
    assert_eq!(p.shown().screen, Indicator { names: vec!["alpheus".into()], unnamed: false });
    assert!(!p.shown().mic.active() && !p.shown().camera.active());
    assert!(!p.set_recording(true, t0 + millis(10)), "no change");
    let cast = Capture { screen: indicator(&["obs"], false), ..Capture::default() };
    p.set_sound(Some(&cast), false, t0 + millis(20));
    assert_eq!(p.shown().screen.names, ["alpheus", "obs"], "both holders");
    p.set_sound(Some(&Capture::default()), false, t0 + millis(30));
    assert!(!p.set_recording(false, t0 + millis(100)), "going off waits the delay");
    assert!(p.shown().screen.active());
    assert_eq!(p.shown().screen.names, ["alpheus"], "obs went, alpheus stays shown");
    assert_eq!(p.deadline(), Some(t0 + millis(100) + OFF_DELAY));
    assert!(!p.due(t0 + millis(100) + OFF_DELAY - millis(1)));
    assert!(p.due(t0 + millis(100) + OFF_DELAY));
    assert!(p.shown().is_empty());
    // The ignore list applies to alpheus as to any name.
    let mut ignoring = Privacy::new(vec!["Alpheus".into()]);
    assert!(!ignoring.set_recording(true, t0));
    assert!(ignoring.shown().is_empty());
}
