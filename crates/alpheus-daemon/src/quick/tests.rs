//! Tests of the quick panel's model (`mod.rs`) and its cells (`draw.rs`): the rows and their
//! order, the selection, every key, click and wheel notch, the sliders' pacing, the confirmation,
//! and what each list's row and detail (Wi-Fi, Bluetooth, VPN, Shield, do-not-disturb) do inside the
//! panel. No file or command, and no waiting: each test takes one `Instant` and adds the time it
//! wants to it, then reads the actions the panel returns.

use super::draw::{LabelForm, session_form, session_label, slider};
use super::*;
use crate::grid::Tint;
use crate::list::Boxed;
use crate::osd::{BRIGHTNESS, MIC_OFF, MIC_ON};
use crate::quick::dnd::DndChoice;
use crate::quick::wifi::{OFF_NOTE, STRENGTH, WIFI_OFF, WIFI_ON};
use crate::segments::BLUETOOTH_CONNECTED;
use crate::segments::VOLUME_MEDIUM;
use crate::system::Volume;
use crate::system::{
    AccessPoint, ActiveVpn, BtDevice, Headset, HeadsetMode, Profile, SavedWifi, Security, VpnKind, VpnProfile, VpnScope, VpnState,
    WifiDevice, WifiState,
};

/// A volume at `percent`, muted or not.
fn a_volume_of(percent: u8, muted: bool) -> Option<Volume> {
    Some(Volume { percent, muted })
}

/// An output (a sink) with pw-dump's node id and label, the default or not.
fn an_output(id: u32, label: &str, default: bool) -> Sink {
    Sink { id, label: label.into(), default }
}

/// Speaker (99) and the default AG06 (122).
fn two_sinks() -> Vec<Sink> {
    vec![an_output(99, "Speaker", false), an_output(122, "AG06/AG03", true)]
}

/// A backlight at `percent`.
fn a_backlight(percent: u8) -> Option<Backlight> {
    Some(Backlight { device: "intel_backlight".into(), percent })
}

/// What the rest of the daemon says, owned, so a test changes one part.
struct World {
    sound: SoundState,
    dnd: Option<bool>,
    dnd_end: Option<(u8, u8)>,
    profile: ProfileState,
    config: QuickConfig,
    bluetooth: Option<BtState>,
    shield: Option<crate::shield::ShieldView>,
}

impl World {
    /// Everything present: the sink at 64 %, the microphone live, do-not-disturb off, the balanced
    /// profile of three, the default config (lock, reboot, power off; no toggle).
    fn full() -> World {
        World {
            sound: SoundState { sink: a_volume_of(64, false), source: a_volume_of(100, false), ..SoundState::default() },
            dnd: Some(false),
            dnd_end: None,
            profile: ProfileState { active: Some(Profile::Balanced), offered: Profile::ALL.to_vec() },
            config: QuickConfig::default(),
            bluetooth: None,
            shield: None,
        }
    }

    fn live(&self) -> Live<'_> {
        Live {
            sound: Some(&self.sound),
            dnd: self.dnd,
            dnd_end: self.dnd_end,
            profile: Some(&self.profile),
            config: &self.config,
            bluetooth: self.bluetooth.as_ref(),
            shield: self.shield,
        }
    }
}

/// The panel as it opens on `w`: two sinks, the backlight at 75 %, toggles unknown, room for 40 rows.
fn panel_opened_on(w: &World, focus: Focus) -> Quick {
    Quick::new(focus, &w.live(), two_sinks(), a_backlight(75), [None, None], 40)
}

/// The text of each row, trailing spaces trimmed.
fn row_texts(b: &Boxed) -> Vec<String> {
    b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
}

/// `s` centred in `w` cells (the left side takes the smaller half).
fn centred(s: &str, w: u16) -> String {
    let pad = usize::from(w.saturating_sub(crate::grid::text_width(s)));
    format!("{}{s}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2))
}

/// The session row's text as `session_cells(34, n)` lays it out, trailing spaces trimmed.
fn session_text(buttons: &[Session]) -> String {
    let mut out = String::from(" ");
    let cells = session_cells(WIDTH + 2, buttons.len());
    let form = session_form(buttons, &cells);
    for (i, (s, (at, w))) in buttons.iter().zip(&cells).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        assert_eq!(text_width(&out), *at);
        out.push_str(&centred(&session_label(*s, form), *w));
    }
    out.trim_end().to_string()
}

#[test]
fn session_cells_split_the_row() {
    assert_eq!(session_cells(34, 1), [(1, 32)]);
    assert_eq!(session_cells(34, 2), [(1, 15), (17, 16)]);
    assert_eq!(session_cells(34, 3), [(1, 10), (12, 10), (23, 10)]);
    assert_eq!(session_cells(34, 4), [(1, 7), (9, 7), (17, 7), (25, 8)]);
}

#[test]
fn every_session_button_takes_the_same_label_form() {
    let all = Session::ALL;
    let widths = |w: u16| -> Vec<(u16, u16)> { all.iter().map(|_| (0, w)).collect() };
    // Room for every glyph and word: all of them carry both.
    assert_eq!(session_form(&all, &widths(10)), LabelForm::GlyphAndWord);
    // "󰍃 Log out" needs 9: at 7 (four buttons in the panel) the words alone, even where "󰌾 Lock" would fit.
    assert_eq!(session_form(&all, &widths(7)), LabelForm::Word);
    assert_eq!(session_form(&all, &widths(2)), LabelForm::Glyph);
    // The row as drawn: no button keeps a glyph the others lost.
    let row = session_text(&all);
    assert!(!row.contains(LOCK) && !row.contains(POWER) && row.contains("Log out"), "{row:?}");
    assert_eq!(session_label(Session::Lock, LabelForm::GlyphAndWord), format!("{LOCK} Lock"));
    assert_eq!(session_label(Session::Poweroff, LabelForm::Word), "Off");
    assert_eq!(session_label(Session::Poweroff, LabelForm::Glyph), POWER.to_string());
    // The last cell takes the remainder, yet the form is the first cells' (the narrowest).
    let four = session_cells(WIDTH + 2, 4);
    assert_eq!(session_form(&all, &four), LabelForm::Word);
}

#[test]
fn a_press_hits_the_button_drawn_under_it() {
    let now = Instant::now();
    let with = |lock: bool, logout: bool, poweroff: bool, reboot: bool| {
        let mut c = QuickConfig::default();
        let set = |on: bool| if on { vec!["fake".to_string()] } else { vec![] };
        c.lock = set(lock);
        c.logout = set(logout);
        c.poweroff = set(poweroff);
        c.reboot = set(reboot);
        c
    };
    let configs = [with(true, false, false, false), with(true, false, true, false), QuickConfig::default(), with(true, true, true, true)];
    for config in configs {
        let mut w = World::full();
        w.config = config;
        let l = w.live();
        let buttons = session::configured(&w.config);
        let cells = session_cells(WIDTH + 2, buttons.len());
        assert_eq!(cells.len(), buttons.len());
        for col in 0..WIDTH + 2 {
            let mut q = panel_opened_on(&w, Focus::Top);
            q.selected = Row::Session;
            let drawn = rows(&l, &q).rows[SESSION_LINE as usize].styles().chars().nth(usize::from(col)).unwrap();
            let got = q.press(&l, (col, SESSION_LINE), Button::Left, now);
            let hit = cells.iter().position(|(at, cw)| col >= *at && col < at + cw);
            assert_eq!(drawn != '.', hit.is_some(), "{} buttons, column {col}: drawn {drawn:?}", buttons.len());
            match hit {
                Some(i) => {
                    assert_eq!(q.button, buttons[i], "{} buttons, column {col}", buttons.len());
                    match buttons[i] {
                        Session::Lock => assert!(matches!(got, QuickAction::RunAndClose { .. }), "lock runs: column {col}"),
                        Session::Logout => assert!(matches!(got, QuickAction::RunAndClose { .. })),
                        _ => {
                            assert_eq!(got, QuickAction::Redraw, "{:?} asks: column {col}", buttons[i]);
                            assert!(q.confirm.is_some());
                        }
                    }
                }
                None => assert_eq!(got, QuickAction::Nothing, "{} buttons, gap column {col}", buttons.len()),
            }
        }
    }
}

#[test]
fn a_gap_press_on_an_unselected_session_row_selects_it() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert_ne!(q.selected, Row::Session);
    let got = q.press(&l, (0, SESSION_LINE), Button::Left, Instant::now());
    assert_eq!(got, QuickAction::Redraw, "the press lands in a gap of the row");
    assert_eq!(q.selected, Row::Session);
    assert!(q.confirm.is_none(), "a gap never asks");
}

#[test]
fn the_session_row_draws_its_cells() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    let b = rows(&l, &q);
    assert_eq!(row_texts(&b)[7], session_text(&[Session::Lock, Session::Reboot, Session::Poweroff]));
    assert_eq!(b.rows[7].styles(), format!(".{}.{}.{}.", "s".repeat(10), "s".repeat(10), "s".repeat(10)));
    assert_eq!(b.rows[7].styles().len(), 34);
    q.selected = Row::Session;
    q.button = Session::Reboot;
    let b = rows(&l, &q);
    assert_eq!(b.rows[7].styles(), format!(".{}.{}.{}.", "s".repeat(10), "A".repeat(10), "s".repeat(10)));
}

#[test]
fn toggle_rows_end_in_a_switch() {
    let mut w = World::full();
    let l = w.live();
    let q = panel_opened_on(&w, Focus::Top);
    let b = rows(&l, &q);
    let dnd = b.rows[4].text();
    assert_eq!(dnd, format!(" {BELL} Do not disturb{} ○ off  ", " ".repeat(9)));
    assert_eq!(b.rows[4].styles()[26..33], *"sssssss");
    w.dnd = Some(true);
    let b = rows(&w.live(), &q);
    assert!(b.rows[4].text().ends_with(" ON  ●  "), "{:?}", b.rows[4].text());
    assert_eq!(b.rows[4].styles()[26..33], *"AAAAAAA");
    let mut c = QuickConfig::default();
    c.night_light.toggle = vec!["fake-night".into()];
    w.config = c;
    let q = panel_opened_on(&w, Focus::Top);
    let b = rows(&w.live(), &q);
    let night = b.rows.iter().find(|g| g.text().contains("Night light")).unwrap();
    assert_eq!(night.styles().chars().nth(32), Some('m'));
    assert_eq!(night.text().chars().nth(32), Some('·'));
    // The microphone: live is on, muted is off.
    let live = rows(&w.live(), &q);
    assert!(live.rows[3].text().contains(" ON  ●"));
    w.sound.source = a_volume_of(100, true);
    let muted = rows(&w.live(), &q);
    assert!(muted.rows[3].text().contains(" ○ off"));
}

#[test]
fn every_row_shows_in_the_spec_order_with_its_separators() {
    let w = World::full();
    let l = w.live();
    let q = panel_opened_on(&w, Focus::Top);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
    assert_eq!(
        lines(&l, &q),
        [
            Line::Row(Row::Volume),
            Line::Row(Row::Brightness),
            Line::Separator,
            Line::Row(Row::Mic),
            Line::Row(Row::Dnd),
            Line::Row(Row::Profile),
            Line::Separator,
            Line::Row(Row::Session)
        ]
    );
    let b = rows(&l, &q);
    assert_eq!(b.cells(), (WIDTH + 2, 8));
    assert_eq!(b.separators, [2, 6]);
    let t = row_texts(&b);
    assert_eq!(t[0], format!(" {VOLUME_MEDIUM} {}{}  64%  ›", "━".repeat(14), "─".repeat(8)), "64 % of 22 cells: 14; › with two sinks");
    assert_eq!(t[1], format!(" {BRIGHTNESS} {}{}  75%", "━".repeat(16), "─".repeat(6)));
    assert_eq!(t[3], format!(" {MIC_ON} Microphone{} ON  ●", " ".repeat(13)), "the microphone is a switch, on while live");
    assert_eq!(t[4], format!(" {BELL} Do not disturb{} ○ off", " ".repeat(9)));
    assert_eq!(t[5], format!(" {PROFILE} Power profile{}balanced ›", " ".repeat(7)));
    assert_eq!(
        t[7],
        format!(
            " {} {} {}",
            centred(&format!("{LOCK} Lock"), 10),
            centred(&format!("{REBOOT} Reboot"), 10),
            centred(&format!("{POWER} Off"), 10)
        )
        .trim_end(),
        "lock, reboot, power off: logout is empty by default"
    );
    // Selected: the band spans the whole row, its track included; the bar cells keep their own
    // foreground on it (a band that skipped the track looked broken).
    let selected = b.rows[0].cells();
    assert!(selected.iter().all(|c| c.style.bg == Tint::Selection), "the band spans the row: {}", b.rows[0].styles());
    assert_eq!((&b.rows[0].styles()[..3], &b.rows[0].styles()[25..]), ("SSS", "S".repeat(9).as_str()));
    let track: String = selected[3..25]
        .iter()
        .map(|c| match c.style.fg {
            Tint::Accent => 'a',
            Tint::FgDim => 'm',
            _ => '?',
        })
        .collect();
    assert_eq!(track, format!("{}{}", "a".repeat(14), "m".repeat(8)), "the bar in its colours, on the band");
    assert_eq!(b.rows[1].styles(), format!(".a.{}{}{}", "a".repeat(16), "m".repeat(6), ".".repeat(9)));
    assert_eq!(&b.rows[4].styles()[26..33], "sssssss", "the off switch on a pill");
}

#[test]
fn the_rows_style_their_switches_and_tracks_in_warn_accent_and_dim() {
    let mut w = World::full();
    w.sound.source = a_volume_of(100, true);
    w.sound.sink = a_volume_of(120, false);
    w.dnd = Some(true);
    w.config.keep_awake.toggle = vec!["fake-keep-awake".into()];
    w.config.night_light.toggle = vec!["fake-night".into()];
    let q = Quick::new(Focus::Top, &w.live(), two_sinks(), a_backlight(75), [Some(true), None], 40);
    let b = rows(&w.live(), &q);
    let t = row_texts(&b);
    // Volume, brightness, separator, mic, DND, profile, keep awake, night light, separator, session.
    assert_eq!(t[3], format!(" {MIC_OFF} Microphone{} ○ off", " ".repeat(13)), "a muted microphone is the off switch");
    assert_eq!(&b.rows[3].styles()[26..33], "sssssss", "no longer warn");
    assert_eq!(t[4], format!(" {BELL} Do not disturb{} ON  ●", " ".repeat(9)));
    assert_eq!(&b.rows[4].styles()[26..33], "AAAAAAA", "ON filled in the accent");
    assert_eq!(t[6], format!(" {KEEP_AWAKE} Keep awake{} ON  ●", " ".repeat(13)));
    assert_eq!(t[7], format!(" {NIGHT_LIGHT} Night light{}·", " ".repeat(18)), "unknown state");
    // The volume row is selected: its track on the band, in its own foreground.
    let track = |b: &Boxed| b.rows[0].cells()[3..25].iter().map(|c| c.style.fg).collect::<Vec<_>>();
    assert!(track(&b).iter().all(|f| *f == Tint::Warn), "above 100 %: the whole bar in warn: {:?}", track(&b));
    let mut muted = World::full();
    muted.sound.sink = a_volume_of(64, true);
    let b = rows(&muted.live(), &panel_opened_on(&muted, Focus::Top));
    assert!(track(&b).iter().all(|f| *f == Tint::FgDim), "muted: the bar dimmed: {:?}", track(&b));
}

#[test]
fn a_row_whose_tool_hardware_or_command_is_missing_is_not_shown() {
    let w = World::full();
    let l = w.live();
    let rows_of = |l: &Live, b: Option<Backlight>| shown(l, &Quick::new(Focus::Top, l, two_sinks(), b, [None, None], 40));
    assert!(!rows_of(&l, None).contains(&Row::Brightness), "no backlight");
    let no_daemon = ProfileState::default();
    assert!(!rows_of(&Live { profile: Some(&no_daemon), ..l }, a_backlight(75)).contains(&Row::Profile), "no power-profiles-daemon");
    assert!(!rows_of(&Live { profile: None, ..l }, a_backlight(75)).contains(&Row::Profile), "the system bus down");
    let no_mic = SoundState { source: None, ..w.sound.clone() };
    assert_eq!(
        rows_of(&Live { sound: Some(&no_mic), ..l }, a_backlight(75))[..3],
        [Row::Volume, Row::Brightness, Row::Dnd],
        "no default source"
    );
    assert_eq!(rows_of(&Live { sound: None, ..l }, a_backlight(75))[0], Row::Brightness, "pw-dump down: no volume, no microphone");
    assert!(!rows_of(&Live { dnd: None, ..l }, a_backlight(75)).contains(&Row::Dnd), "no notifications module");
    let mut c = QuickConfig { lock: vec![], reboot: vec![], poweroff: vec![], ..QuickConfig::default() };
    assert!(!rows_of(&Live { config: &c, ..l }, a_backlight(75)).contains(&Row::Session), "every session command empty");
    c.logout = vec!["fake-logout".into()];
    let one = Live { config: &c, ..l };
    let b = rows(&one, &Quick::new(Focus::Top, &one, two_sinks(), None, [None, None], 40));
    assert_eq!(row_texts(&b).last().cloned(), Some(session_text(&[Session::Logout])), "one button: log out");
    let k =
        QuickConfig { keep_awake: ToggleConfig { toggle: vec!["fake-keep-awake".into()], state: String::new() }, ..QuickConfig::default() };
    assert_eq!(
        rows_of(&Live { config: &k, ..l }, None)[4..],
        [Row::KeepAwake, Row::Session],
        "a configured toggle shows; the other stays hidden"
    );
}

/// Nothing at all to show.
#[test]
fn with_nothing_to_show_the_panel_is_empty() {
    let c = QuickConfig { lock: vec![], reboot: vec![], poweroff: vec![], ..QuickConfig::default() };
    let l = Live { sound: None, dnd: None, dnd_end: None, profile: None, config: &c, bluetooth: None, shield: None };
    let q = Quick::new(Focus::Sound, &l, Vec::new(), None, [None, None], 40);
    assert!(q.is_empty(&l), "nothing to open: the wiring logs it and opens nothing");
    assert!(lines(&l, &q).is_empty());
    assert_eq!(rows(&l, &q).cells(), (0, 0));
    let w = World::full();
    assert!(!panel_opened_on(&w, Focus::Top).is_empty(&w.live()));
}

#[test]
fn the_section_asked_for_is_selected_at_open() {
    assert_eq!(Focus::of(&Target::Module(ModuleId::Volume)), Focus::Sound);
    assert_eq!(Focus::of(&Target::Module(ModuleId::Network)), Focus::Wifi, "quick Wi-Fi spec § 1");
    assert_eq!(
        panel_opened_on(&World::full(), Focus::Wifi).selected,
        Row::Volume,
        "no Wi-Fi state read yet, so no Wi-Fi row: the first row"
    );
    assert_eq!(Focus::of(&Target::Module(ModuleId::Bluetooth)), Focus::Bluetooth, "quick Bluetooth spec § 1");
    let w = World::full();
    assert_eq!(panel_opened_on(&w, Focus::Sound).selected, Row::Volume);
    assert_eq!(panel_opened_on(&w, Focus::Top).selected, Row::Volume, "the volume is the first row");
    let l = Live { sound: None, ..w.live() };
    assert_eq!(
        Quick::new(Focus::Sound, &l, two_sinks(), a_backlight(75), [None, None], 40).selected,
        Row::Brightness,
        "no volume row: the first"
    );
}

/// Quick Bluetooth spec § 1: a click on the Bluetooth module, or `--quick bluetooth`, opens its
/// detail; Enter on the row too, Escape goes back to the row.
#[test]
fn focus_bluetooth_opens_the_detail() {
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(80) }] });
    let q = panel_opened_on(&w, Focus::Bluetooth);
    assert!(q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth);
    assert_eq!(lines(&w.live(), &q)[..2], [Line::BtHeader, Line::Separator]);
    assert_eq!(q.bt_scope(), BtScope::Detail);
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Bluetooth, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
    assert_eq!(row_texts(&rows(&l, &q))[3], format!(" {BLUETOOTH_CONNECTED} Bluetooth{}CasqueBT ›", " ".repeat(11)));
    q.selected = Row::Bluetooth;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "no view read yet: no discovery");
    assert!(q.bt.open);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth, "back on its row");
    w.bluetooth = Some(BtState::Absent);
    let q = panel_opened_on(&w, Focus::Bluetooth);
    assert!(!q.bt.open, "no Bluetooth row: nothing opens");
    assert_eq!(q.selected, Row::Volume, "the first row");
}

/// The D-Bus thread's view while the detail is open: discovery, BlueZ lost, the row gone.
#[test]
fn the_bluetooth_detail_follows_its_view() {
    use crate::quick::bt::BtOrder;
    use crate::sources::dbus::bt_act::ActCall;
    use crate::system::{BtAdapter, BtEntry, BtKind, BtView};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let adapter = "/org/bluez/hci0";
    let view = || BtView {
        adapter: Some(BtAdapter { path: adapter.into(), powered: true, blocked: false }),
        paired: vec![BtEntry {
            path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01".into(),
            label: "CasqueBT".into(),
            kind: BtKind::Headset,
            paired: true,
            trusted: true,
            connected: false,
        }],
        nearby: vec![],
    };
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Bluetooth);
    assert_eq!(
        q.follow_bt(&w.live(), Some(view()), t),
        QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::StartDiscovery { adapter: adapter.into() })])
    );
    assert!(q.bt.is_selected(Line::BtDevice(0), &q.bt.devices()), "the first device");
    assert_eq!(q.follow_bt(&w.live(), None, t), QuickAction::Redraw, "BlueZ lost: its orders dropped");
    assert!(!q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth, "back on the row");
    let mut q = panel_opened_on(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(view()), t);
    w.bluetooth = Some(BtState::Absent);
    q.fit(&w.live());
    assert!(!q.bt.open, "the row gone: the detail closes");
    assert_eq!(q.selected, Row::Mic, "the next row down");
}

/// A Bluetooth view with ClavierBT paired, and Keyboard-Test nearby.
fn clavier_view() -> BtView {
    use crate::system::{BtAdapter, BtEntry, BtKind};
    let entry = |n: u8, label: &str, paired: bool| BtEntry {
        path: format!("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_{n:02X}"),
        label: label.into(),
        kind: BtKind::Keyboard,
        paired,
        trusted: paired,
        connected: false,
    };
    BtView {
        adapter: Some(BtAdapter { path: "/org/bluez/hci0".into(), powered: true, blocked: false }),
        paired: vec![entry(2, "ClavierBT", true)],
        nearby: vec![entry(3, "Keyboard-Test", false)],
    }
}

/// A double Enter (or a held one) on a device makes one call.
#[test]
fn a_double_enter_on_a_device_calls_once() {
    use crate::quick::bt::BtOrder;
    use crate::sources::dbus::bt_act::ActCall;
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let connect = ActCall::Connect { device: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into() };
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Bluetooth(vec![BtOrder::Call(connect)]));
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing, "auto-repeat");
    assert_eq!(q.key(&l, Key::Enter, false, t + Duration::from_millis(50)), QuickAction::Nothing, "during …");
}

/// A held arrow while a code question shows repeats into nothing: the question that just
/// arrived is never rejected by a key pressed before it.
#[test]
fn a_held_key_never_answers_a_code_question() {
    use crate::quick::bt::{BtOrder, Code};
    use crate::sources::dbus::bt_act::{ActCall, AgentId, AgentReply, AgentRequest};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let pairing = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03".to_string();
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::Pair { device: pairing.clone() })]));
    let id = AgentId(4);
    q.bt_agent(id, AgentRequest::RequestConfirmation { device: pairing.clone(), passkey: 482_913 }, t);
    for k in [Key::Down, Key::Up, Key::Left, Key::Right] {
        assert_eq!(q.key(&l, k, true, t), QuickAction::Nothing, "{k:?} held");
    }
    assert!(matches!(q.bt.code, Some(Code::Question { .. })), "still asked");
    assert_eq!(
        q.key(&l, Key::Down, false, t),
        QuickAction::Bluetooth(vec![BtOrder::Reply(id, AgentReply::Rejected), BtOrder::Forget(ActCall::Pair { device: pairing })]),
        "a fresh press still answers no"
    );
}

/// Spec § 3: a code question cancels a pending `Forget?`; leaving drops it too.
#[test]
fn the_question_cancels_forget() {
    use crate::quick::bt::BtOrder;
    use crate::sources::dbus::bt_act::{ActCall, AgentId, AgentReply, AgentRequest};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let pairing = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03".to_string();
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::Pair { device: pairing.clone() })]));
    q.confirm = Some((Question::Forget("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into()), t + CONFIRM));
    assert_eq!(q.bt_agent(AgentId(1), AgentRequest::RequestConfirmation { device: pairing.clone(), passkey: 1 }, t), QuickAction::Redraw);
    assert_eq!(q.confirm, None);
    q.confirm = Some((Question::Forget("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into()), t + CONFIRM));
    assert_eq!(
        q.bt_leaving(),
        vec![
            BtOrder::Reply(AgentId(1), AgentReply::Rejected),
            BtOrder::Call(ActCall::CancelPairing { device: pairing.clone() }),
            BtOrder::Forget(ActCall::Pair { device: pairing }),
            BtOrder::Call(ActCall::StopDiscovery { adapter: "/org/bluez/hci0".into() }),
        ]
    );
    assert_eq!(q.confirm, None, "leaving drops a pending Forget");
    q.confirm = Some((Question::Session(Session::Reboot), t + CONFIRM));
    q.bt_leaving();
    assert!(q.confirm.is_some(), "not a session question");
}

/// The Bluetooth timers join the panel's: the earliest deadline, and what `due` finds.
#[test]
fn bluetooth_deadlines_and_logs_go_through_due() {
    use crate::quick::bt::{BtOrder, CALL_DEADLINE};
    use crate::sources::dbus::bt_act::ActCall;
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    q.key(&w.live(), Key::Enter, false, t);
    assert_eq!(q.deadline(), Some(t + CALL_DEADLINE));
    let connect = ActCall::Connect { device: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into() };
    assert_eq!(
        q.due(t + CALL_DEADLINE),
        Due {
            redraw: true,
            bt_orders: vec![BtOrder::Forget(connect)],
            bt_logs: vec!["quick: bluetooth connect failed (timeout)".into()],
            ..Due::default()
        }
    );
    let (a, log) = q.bt_answered(
        &ActCall::StartDiscovery { adapter: "/org/bluez/hci0".into() },
        Err(crate::sources::dbus::bt_act::ActError::Remote("org.bluez.Error.InProgress".into())),
        t,
    );
    assert_eq!((a, log.as_deref()), (QuickAction::Nothing, Some("quick: bluetooth discovery failed (org.bluez.Error.InProgress)")));
}

/// Spec § 2: a left press on the device asked about is the forget question's second activation;
/// a press on any other line cancels it.
#[test]
fn a_press_on_the_asking_device_confirms_forget() {
    use crate::quick::bt::BtOrder;
    use crate::sources::dbus::bt_act::ActCall;
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let clavier = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".to_string();
    let row = |q: &Quick, line: Line| lines(&l, q).iter().position(|x| *x == line).unwrap() as u16;
    assert_eq!(q.key(&l, Key::Delete, false, t), QuickAction::Redraw);
    assert_eq!(q.press(&l, (3, row(&q, Line::BtNearby)), Button::Left, t), QuickAction::Redraw, "another line: cancelled");
    assert_eq!(q.confirm, None);
    q.key(&l, Key::Delete, false, t);
    assert_eq!(q.confirm, Some((Question::Forget(clavier.clone()), t + CONFIRM)));
    assert_eq!(
        q.press(&l, (3, row(&q, Line::BtDevice(0))), Button::Left, t + CONFIRM_AFTER),
        QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::RemoveDevice { adapter: "/org/bluez/hci0".into(), device: clavier })])
    );
}

/// A left click on the Bluetooth row opens its detail.
#[test]
fn a_left_click_on_the_bluetooth_row_opens_the_detail() {
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    let at = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Bluetooth)).unwrap();
    assert_eq!(q.press(&l, (3, at as u16), Button::Left, Instant::now()), QuickAction::Redraw, "no view read yet");
    assert!(q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth);
}

/// Spec § 2: Space on the Bluetooth row is a power action with its 30 s deadline.
#[test]
fn the_rows_power_toggle_is_armed() {
    use crate::quick::bt::{CALL_DEADLINE, Step};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.bt.view = Some(clavier_view());
    q.selected = Row::Bluetooth;
    q.key(&w.live(), Key::Char(' '), false, t);
    assert_eq!(q.bt.action.as_ref().map(|a| (a.step, a.deadline)), Some((Step::Power, Some(t + CALL_DEADLINE))));
}

#[test]
fn the_selection_stays_on_its_row_when_rows_come_and_go() {
    let mut w = World::full();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Mic;
    w.sound.source = None;
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Dnd, "the microphone unplugged: the next row down");
    w.sound.source = a_volume_of(100, false);
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Dnd, "it stays where it went: by identity, never by line");
    q.selected = Row::Session;
    w.config.lock = vec![];
    w.config.reboot = vec![];
    w.config.poweroff = vec![];
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Profile, "the last row gone: the one above");
    let mut w = World::full();
    w.sound.source = None;
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Dnd;
    w.sound.source = a_volume_of(100, false);
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Dnd, "a row appearing above never moves it");
}

#[test]
fn arrows_home_and_end_move_over_the_rows_never_wrapping() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Nothing, "the first row: clamped");
    assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Brightness);
    assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Mic, "over the separator");
    assert_eq!(q.key(&l, Key::End, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Session);
    assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Nothing, "never wrapping");
    assert_eq!(q.key(&l, Key::CtrlP, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Profile);
    assert_eq!(q.key(&l, Key::Home, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Volume);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Close);
}

/// A held Enter opens nothing more; the arrows repeat.
#[test]
fn a_held_space_or_enter_acts_once() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing, "a repeated Enter: nothing");
    assert!(!q.detail);
    assert_eq!(q.key(&l, Key::Down, true, t), QuickAction::Redraw, "the arrows repeat");
    assert_eq!(q.key(&l, Key::Up, true, t), QuickAction::Redraw);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "pressed: the outputs open");
    assert!(q.detail);
}

#[test]
fn the_outputs_detail_lists_every_sink_and_sets_the_default() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert!(q.detail);
    assert_eq!(q.sink, Some(122), "the default chosen at open");
    let b = rows(&l, &q);
    assert_eq!(row_texts(&b), [" ‹ Outputs".to_string(), String::new(), "   Speaker".to_string(), " ● AG06/AG03".to_string()]);
    assert_eq!(b.separators, [1]);
    assert_eq!(&b.rows[3].styles()[..3], "SSS", "the chosen sink on the band");
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Redraw);
    assert_eq!(q.sink, Some(99));
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Nothing, "clamped");
    assert_eq!(
        q.key(&l, Key::Enter, false, t),
        QuickAction::Run { argv: argv(&["wpctl", "set-default", "99"]), what: "default sink 99".into() }
    );
    assert!(q.detail, "the detail stays, the ● follows pw-dump");
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.detail);
    assert_eq!(q.selected, Row::Volume, "back on the volume row");
    q.key(&l, Key::Enter, false, t);
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw, "← leaves it too");
    assert!(!q.detail);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Close, "from the rows, Escape closes");
}

#[test]
fn a_single_sink_draws_no_arrow_and_opens_no_detail() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = Quick::new(Focus::Sound, &l, vec![an_output(122, "AG06/AG03", true)], a_backlight(75), [None, None], 40);
    assert!(row_texts(&rows(&l, &q))[0].ends_with("64%"), "no ›");
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Nothing);
    assert!(!q.detail);
    let mut q = panel_opened_on(&w, Focus::Sound);
    q.key(&l, Key::Enter, false, t);
    q.set_sinks(vec![an_output(122, "AG06/AG03", true)]);
    q.fit(&l);
    assert!(!q.detail, "a sink unplugged under the detail: back to the rows");
}

#[test]
fn the_detail_keeps_its_sink_by_id() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Sound);
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Up, false, t);
    assert_eq!(q.sink, Some(99));
    q.set_sinks(vec![an_output(64, "HDMI", false), an_output(99, "Speaker", false), an_output(122, "AG06/AG03", true)]);
    q.fit(&l);
    assert_eq!(q.sink, Some(99), "a sink arriving above it moves its line, never the choice");
    assert_eq!(&rows(&l, &q).rows[3].styles()[..3], "SSS", "Speaker is now on line 3");
    q.set_sinks(vec![an_output(64, "HDMI", false), an_output(122, "AG06/AG03", true)]);
    q.fit(&l);
    assert_eq!(q.sink, Some(122), "its sink gone: the default");
}

#[test]
fn the_window_follows_the_selection_when_the_lines_do_not_fit() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), a_backlight(75), [None, None], 3);
    assert_eq!(rows(&l, &q).rows.len(), 3, "three rows of room");
    q.key(&l, Key::End, false, t);
    assert_eq!(q.top, 5, "8 lines, 3 shown: the last three");
    assert_eq!(
        row_texts(&rows(&l, &q)).last().cloned(),
        Some(session_text(&[Session::Lock, Session::Reboot, Session::Poweroff])),
        "the session row in view"
    );
    q.key(&l, Key::Home, false, t);
    assert_eq!(q.top, 0);
}

#[test]
fn re_reads_replace_the_backlight_and_the_toggles() {
    let w = World::full();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_backlight(Reread::Level(40));
    assert_eq!(q.backlight, a_backlight(40));
    q.set_backlight(Reread::Unreadable);
    assert_eq!(q.backlight, a_backlight(40), "one failed read: the row and its last level stay");
    q.set_backlight(Reread::Gone);
    assert!(!shown(&w.live(), &q).contains(&Row::Brightness), "the device went: its row with it");
    q.set_toggle(Toggle::NightLight, Some(true));
    assert_eq!(q.toggles, [None, Some(true)]);
}

const DND_LINE: u16 = 4;
const SESSION_LINE: u16 = 7;

/// The `i`-th instant a slider may send again: each a full COALESCE after the one before.
fn later(t: Instant, i: u32) -> Instant {
    t + COALESCE * i
}

fn set_volume(n: u8) -> QuickAction {
    run(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", &format!("{n}%")], &format!("volume {n}%"))
}

fn set_brightness(n: u8) -> QuickAction {
    run(&["brightnessctl", "--class=backlight", "--device=intel_backlight", "set", &format!("{n}%")], &format!("brightness {n}%"))
}

/// Every volume command is an absolute level the model computed from what it shows.
#[test]
fn volume_steps_mutes_and_takes_a_click_on_its_bar() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Sound);
    let t = Instant::now();
    let mute = run(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"], "mute");
    assert_eq!(q.key(&l, Key::Right, false, later(t, 0)), set_volume(69), "64 % + 5");
    assert!(row_texts(&rows(&l, &q))[0].contains(" 69%"), "the expected level shown at once");
    assert_eq!(q.key(&l, Key::Left, false, later(t, 1)), set_volume(64), "from the level shown, not pw-dump's");
    assert_eq!(q.key(&l, Key::Right, true, later(t, 2)), set_volume(69), "← → repeat");
    assert_eq!(q.key(&l, Key::Char(' '), false, later(t, 2)), mute);
    assert_eq!(q.press(&l, (BAR_START, 0), Button::Left, later(t, 3)), set_volume(4), "the bar's first cell");
    assert_eq!(q.press(&l, (BAR_START + BAR - 1, 0), Button::Left, later(t, 4)), set_volume(100), "its last cell");
    assert_eq!(q.press(&l, (BAR_START + 10, 0), Button::Left, later(t, 5)), set_volume(50), "(10 + 1) × 100 / 22");
    assert_eq!(q.press(&l, (1, 0), Button::Right, later(t, 5)), mute, "a right click on the volume: mute");
    assert_eq!(q.scroll(&l, Some((5, 0)), -1, later(t, 6)), set_volume(55), "the wheel up: louder");
    assert_eq!(q.scroll(&l, Some((5, 0)), 2, later(t, 7)), set_volume(45), "two notches, one command");
    assert_eq!(q.press(&l, (MORE_AT, 0), Button::Left, later(t, 8)), QuickAction::Redraw, "› opens the outputs");
    assert!(q.detail);
    let ten = World { config: QuickConfig { step: 10, ..QuickConfig::default() }, ..World::full() };
    let mut q = panel_opened_on(&ten, Focus::Sound);
    assert_eq!(q.key(&ten.live(), Key::Right, false, t), set_volume(74), "[quick] step");
    let mut loud = World::full();
    loud.sound.sink = a_volume_of(97, false);
    let mut q = panel_opened_on(&loud, Focus::Sound);
    assert_eq!(q.key(&loud.live(), Key::Right, false, t), set_volume(100), "never beyond 100 %");
}

/// A press on the cell either side of the bar is not a level: like any other press on the row, it
/// opens the outputs.
#[test]
fn a_click_beside_the_volume_bar_sets_no_level() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    for col in [BAR_START - 1, BAR_START + BAR] {
        let mut q = panel_opened_on(&w, Focus::Sound);
        assert_eq!(q.press(&l, (col, 0), Button::Left, t), QuickAction::Redraw, "column {col}: no level");
        assert!(q.detail, "column {col} opens the outputs");
    }
}

/// A held → sends at most one command per COALESCE, each the latest level; a burst
/// inside one window is two commands, the second the level the burst ended on.
#[test]
fn a_held_slider_key_sends_the_latest_level_once_per_window() {
    let mut w = World::full();
    w.sound.sink = a_volume_of(30, false);
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Sound);
    let t = Instant::now();
    let burst: Vec<QuickAction> = (0..10).map(|i| q.key(&l, Key::Right, i > 0, t + Duration::from_millis(15 * i))).collect();
    assert_eq!(burst[0], set_volume(35), "the first goes at once");
    assert!(burst[1..].iter().all(|a| *a == QuickAction::Redraw), "the rest wait: {burst:?}");
    assert!(row_texts(&rows(&l, &q))[0].contains(" 80%"), "the level shown follows every key");
    assert_eq!(q.deadline(), Some(t + COALESCE), "the window's end");
    assert_eq!(q.due(t + COALESCE), Due { run: vec![set_volume(80)], ..Due::default() }, "the latest level, once");
    assert_eq!(q.deadline(), Some(t + COALESCE * 2), "a new window after it");
    assert_eq!(q.due(t + COALESCE * 2), Due::default(), "quiet, pw-dump still at 30 %: the level sent stays");
    assert!(row_texts(&rows(&l, &q))[0].contains(" 80%"));
    assert_eq!(q.deadline(), Some(t + COALESCE * 2 + SETTLE), "for SETTLE at most");
    assert_eq!(q.due(t + COALESCE * 2 + SETTLE), Due { redraw: true, ..Due::default() }, "then pw-dump's level");
    assert_eq!(q.deadline(), None, "nothing pending: no timer");
    assert!(row_texts(&rows(&l, &q))[0].contains(" 30%"), "pw-dump's own level again");
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Brightness;
    let burst: Vec<QuickAction> = (0..10).map(|i| q.key(&l, Key::Right, i > 0, t + Duration::from_millis(15 * i))).collect();
    assert_eq!(burst[0], set_brightness(80), "75 % + 5");
    assert!(burst[1..].iter().all(|a| *a == QuickAction::Redraw), "{burst:?}");
    assert_eq!(q.due(t + COALESCE), Due { run: vec![set_brightness(100)], ..Due::default() });
    assert_eq!(q.due(t + COALESCE * 2), Due { backlight: true, ..Due::default() }, "quiet: the re-read");
    assert_eq!(q.key(&l, Key::Left, false, t + COALESCE * 3), set_brightness(95), "a new window: at once");
    assert_eq!(q.key(&l, Key::Left, true, t + COALESCE * 3), QuickAction::Redraw);
    assert_eq!(q.flush(), vec![set_brightness(90)], "closing sends what waits");
    assert_eq!(q.flush(), vec![], "once");
}

/// wpctl and pw-dump (or brightnessctl) slower than a window. The level sent stays
/// until the live source reports it, or SETTLE passes: a stale reading never makes the next
/// press step backwards.
#[test]
fn a_stale_live_level_never_makes_the_next_press_step_backwards() {
    let mut w = World::full();
    w.sound.sink = a_volume_of(30, false);
    let mut q = panel_opened_on(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&w.live(), Key::Right, false, t), set_volume(35));
    assert_eq!(q.key(&w.live(), Key::Right, true, t + Duration::from_millis(10)), QuickAction::Redraw);
    assert_eq!(q.due(later(t, 1)).run, vec![set_volume(40)]);
    w.sound.sink = a_volume_of(35, false); // pw-dump one command behind
    q.fit(&w.live());
    assert_eq!(q.due(later(t, 2)), Due::default(), "quiet, pw-dump still behind");
    q.fit(&w.live());
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 2)), set_volume(45), "from 40 % shown, never 35 %");
    q.due(later(t, 3));
    w.sound.sink = a_volume_of(45, false);
    q.fit(&w.live());
    assert_eq!(q.volume_level, None, "pw-dump caught up: its level from now on");
    assert_eq!(q.deadline(), None, "and nothing left to wait for");
    q.selected = Row::Brightness;
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 4)), set_brightness(80));
    assert_eq!(q.due(later(t, 5)), Due { backlight: true, ..Due::default() });
    q.set_backlight(Reread::Level(75)); // read before brightnessctl wrote
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 5)), set_brightness(85), "from 80 % shown, never 75 %");
}

/// The default sink switched while a level is held or waiting: that level was the
/// old sink's; it is dropped, never sent to the new one.
#[test]
fn a_default_sink_switch_drops_the_held_volume() {
    let mut w = World::full();
    w.sound.sink_name = Some(crate::system::DeviceName { node: "alsa_output.ag06".into(), label: "AG06/AG03".into() });
    let mut q = panel_opened_on(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&w.live(), Key::Right, false, t), set_volume(69));
    assert_eq!(q.key(&w.live(), Key::Right, true, t + Duration::from_millis(10)), QuickAction::Redraw, "74 % waits");
    w.sound.sink_name = Some(crate::system::DeviceName { node: "alsa_output.speaker".into(), label: "Speaker".into() });
    w.sound.sink = a_volume_of(40, false);
    q.fit(&w.live());
    assert_eq!(q.due(later(t, 1)).run, vec![], "never sent to the new sink");
    assert_eq!(q.flush(), vec![]);
    assert!(row_texts(&rows(&w.live(), &q))[0].contains(" 40%"), "the new sink's level");
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 2)), set_volume(45), "stepped from it");
}

#[test]
fn brightness_shows_the_expected_level_until_its_re_read() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Brightness;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Right, false, t), set_brightness(80));
    assert_eq!(q.backlight, a_backlight(80), "the expected level at once");
    assert_eq!(q.deadline(), Some(t + BRIGHTNESS_REREAD));
    assert_eq!(q.due(t + Duration::from_millis(149)), Due::default(), "not yet");
    assert_eq!(q.due(t + BRIGHTNESS_REREAD), Due { backlight: true, ..Due::default() });
    q.set_backlight(Reread::Level(80));
    assert_eq!(q.deadline(), None, "the re-read agrees: nothing pending, no timer");
    q.key(&l, Key::Left, false, later(t, 1));
    q.due(later(t, 2));
    q.set_backlight(Reread::Level(78));
    assert_eq!(q.backlight, a_backlight(75), "a re-read that disagrees is kept back for SETTLE");
    assert_eq!(q.due(later(t, 2) + SETTLE), Due { backlight: true, ..Due::default() }, "one more re-read");
    q.set_backlight(Reread::Level(78));
    assert_eq!(q.backlight, a_backlight(78), "then the re-read wins");
    assert_eq!(q.deadline(), None);
    q.backlight = a_backlight(98);
    q.key(&l, Key::Right, false, later(t, 2));
    assert_eq!(q.backlight, a_backlight(100), "never above 100");
    assert_eq!(q.press(&l, (BAR_START + 10, 1), Button::Left, later(t, 3)), set_brightness(50), "a click on its bar");
    assert_eq!(q.scroll(&l, Some((5, 1)), -2, later(t, 4)), set_brightness(60));
}

/// The panel never takes the backlight below 1 % (a black screen), by a key,
/// the wheel or a click.
#[test]
fn brightness_never_goes_below_one_percent() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Brightness;
    let t = Instant::now();
    q.backlight = a_backlight(3);
    assert_eq!(q.key(&l, Key::Left, false, later(t, 0)), set_brightness(1), "3 % − 5: 1 %");
    assert_eq!(q.backlight, a_backlight(1));
    assert_eq!(q.key(&l, Key::Left, false, later(t, 1)), set_brightness(1), "at 1 %: stays");
    q.backlight = a_backlight(40);
    assert_eq!(q.scroll(&l, Some((5, 1)), 10, later(t, 2)), set_brightness(1), "ten notches down");
    assert_eq!(q.press(&l, (BAR_START, 1), Button::Left, later(t, 3)), set_brightness(4), "the bar's first cell");
    q.backlight = a_backlight(0);
    assert_eq!(q.key(&l, Key::Right, false, later(t, 4)), set_brightness(5), "from a dark backlight, up");
}

/// Fix m4: the wheel counts ten notches at most.
#[test]
fn the_wheel_takes_ten_notches_at_most() {
    let mut w = World::full();
    w.sound.sink = a_volume_of(30, false);
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.scroll(&l, Some((5, 0)), -15, t), set_volume(80), "15 notches of 5: 50, not 75");
    q.selected = Row::Brightness;
    q.backlight = a_backlight(80);
    assert_eq!(q.scroll(&l, Some((5, 1)), 15, t), set_brightness(30), "15 notches down: 50");
}

/// Fix m3: `BAR` is the width `osd::level` draws, from `BAR_START` (the click mapping counts on it).
#[test]
fn the_drawn_bar_spans_bar_start_and_its_bar_cells() {
    let mut g = Grid::empty(WIDTH + 2);
    slider(&mut g, BRIGHTNESS, Volume { percent: 100, muted: false }, false, false);
    let styles: Vec<char> = g.styles().chars().collect();
    let (first, end) = (usize::from(BAR_START), usize::from(BAR_START + BAR));
    assert!(styles[first..end].iter().all(|c| *c == 'a'), "{}", g.styles());
    assert_ne!(styles[first - 1], 'a', "the cell before the bar: {}", g.styles());
    assert_ne!(styles[end], 'a', "the cell after it: {}", g.styles());
}

#[test]
fn the_toggles_run_their_command_and_re_read_their_state() {
    let mut w = World::full();
    w.config.keep_awake.toggle = vec!["fake-keep-awake".into()];
    w.config.night_light.toggle = vec!["fake-night".into(), "--toggle".into()];
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    let t = Instant::now();
    let mic = run(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"], "microphone mute");
    q.selected = Row::Mic;
    assert_eq!(q.key(&l, Key::Char(' '), false, t), mic);
    assert_eq!(q.key(&l, Key::Enter, false, t), mic);
    q.selected = Row::Dnd;
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::ToggleDnd, "set_dnd's one path, in the wiring");
    assert_eq!(q.key(&l, Key::Char(' '), true, t), QuickAction::Nothing, "a held Space acts once");
    q.selected = Row::KeepAwake;
    assert_eq!(q.key(&l, Key::Enter, false, t), run(&["fake-keep-awake"], "keep awake"), "run as given");
    q.selected = Row::NightLight;
    assert_eq!(q.key(&l, Key::Char(' '), false, t + Duration::from_millis(100)), run(&["fake-night", "--toggle"], "night light"));
    assert_eq!(q.deadline(), Some(t + TOGGLE_REREAD), "keep awake's re-read comes first");
    assert_eq!(q.due(t + TOGGLE_REREAD), Due { toggles: [true, false], ..Due::default() });
    assert_eq!(q.due(t + TOGGLE_REREAD + Duration::from_millis(100)), Due { toggles: [false, true], ..Due::default() });
    q.selected = Row::Volume;
    assert_eq!(q.press(&l, (10, 4), Button::Left, t), QuickAction::ToggleDnd, "a left click on a row is Enter");
    assert_eq!(q.selected, Row::Dnd);
}

#[test]
fn enter_cycles_the_power_profile_and_space_does_not() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Profile;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), run(&["powerprofilesctl", "set", "performance"], "profile performance"));
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Nothing, "space does not cycle the profile");
    let top = ProfileState { active: Some(Profile::Performance), offered: Profile::ALL.to_vec() };
    assert_eq!(
        q.key(&Live { profile: Some(&top), ..l }, Key::Enter, false, t),
        run(&["powerprofilesctl", "set", "power-saver"], "profile power-saver"),
        "wrapping"
    );
    let two = ProfileState { active: Some(Profile::Balanced), offered: vec![Profile::PowerSaver, Profile::Balanced] };
    assert_eq!(
        q.key(&Live { profile: Some(&two), ..l }, Key::Enter, false, t),
        run(&["powerprofilesctl", "set", "power-saver"], "profile power-saver"),
        "only what the daemon offers"
    );
}

#[test]
fn reboot_and_power_off_ask_first_and_the_question_expires() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Session;
    let t = Instant::now();
    let lock = QuickAction::RunAndClose { argv: argv(&["loginctl", "lock-session"]), what: "lock".into() };
    assert_eq!(q.key(&l, Key::Enter, false, t), lock, "lock: at once");
    q.button = Session::Reboot;
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "the first activation asks");
    assert_eq!(q.confirm, Some((Question::Session(Session::Reboot), t + CONFIRM)));
    let b = rows(&l, &q);
    assert_eq!(row_texts(&b)[7], format!(" {REBOOT} Reboot? ⏎"));
    assert_eq!(b.rows[7].styles()[1..12], "w".repeat(11), "in warn");
    assert_eq!(q.deadline(), Some(t + CONFIRM), "the timer waits for it");
    let early = t + Duration::from_millis(200);
    assert_eq!(q.key(&l, Key::Enter, false, early), QuickAction::Nothing, "a double activation never confirms");
    assert_eq!(q.press(&l, (20, SESSION_LINE), Button::Left, early), QuickAction::Nothing, "nor a double click");
    assert_eq!(q.confirm, Some((Question::Session(Session::Reboot), t + CONFIRM)), "the question stays, its 3 s unchanged");
    assert_eq!(
        q.key(&l, Key::Enter, false, t + Duration::from_millis(500)),
        QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
        "0.5 s later: it runs"
    );
    q.button = Session::Reboot;
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert_eq!(
        q.key(&l, Key::Enter, false, t + Duration::from_secs(2)),
        QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
        "the second within 3 s runs it"
    );
    q.button = Session::Poweroff;
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert_eq!(q.due(t + CONFIRM), Due { redraw: true, ..Due::default() }, "3 s later the question goes");
    assert_eq!(q.confirm, None);
    assert_eq!(q.key(&l, Key::Enter, false, t + CONFIRM), QuickAction::Redraw, "the next Enter asks again");
    assert_eq!(row_texts(&rows(&l, &q))[7], format!(" {POWER} Power off? ⏎"));
}

#[test]
fn moving_away_cancels_the_confirmation_and_coming_back_asks_again() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Session;
    q.button = Session::Reboot;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw, "← moves to lock");
    assert_eq!((q.confirm.clone(), q.button), (None, Session::Lock), "and cancels");
    q.key(&l, Key::Right, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "back on reboot: it asks again, never runs");
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Redraw);
    assert_eq!(q.confirm, None, "↑ cancels too");
    q.selected = Row::Session;
    q.key(&l, Key::Enter, false, t);
    assert_eq!(q.press(&l, (10, DND_LINE), Button::Left, t), QuickAction::ToggleDnd, "a press on another row");
    assert_eq!(q.confirm, None, "cancels it");
    assert_eq!(
        q.press(&l, (session_cells(WIDTH + 2, 3)[1].0, SESSION_LINE), Button::Left, t),
        QuickAction::Redraw,
        "a press on reboot asks"
    );
    assert_eq!(
        q.press(&l, (20, SESSION_LINE), Button::Left, t + CONFIRM_AFTER),
        QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
        "anywhere on the question: the second activation"
    );
}

#[test]
fn the_wheel_elsewhere_moves_the_window_only_when_the_lines_do_not_fit() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert_eq!(q.scroll(&l, Some((10, 4)), 1, t), QuickAction::Nothing, "everything fits");
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), a_backlight(75), [None, None], 3);
    assert_eq!(q.scroll(&l, Some((10, 2)), 1, t), QuickAction::Redraw, "on the separator, three rows of room");
    assert_eq!(q.top, 1);
    assert_eq!(q.scroll(&l, Some((10, 2)), 10, t), QuickAction::Redraw);
    assert_eq!(q.top, 5, "clamped: the last three lines");
    assert_eq!(q.scroll(&l, Some((10, 2)), -1, t), QuickAction::Redraw);
    assert_eq!(q.top, 4);
    assert_eq!(q.scroll(&l, None, 0, t), QuickAction::Nothing);
}

#[test]
fn a_program_that_fails_is_noted_once_per_open() {
    let w = World::full();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert!(q.note_failure("wpctl"));
    assert!(!q.note_failure("wpctl"), "logged once");
    assert!(q.note_failure("brightnessctl"));
}

const HOME: &str = "11111111-2222-4333-8444-555555555555";

fn home() -> AccessPoint {
    AccessPoint { ssid: b"Maison-Test".to_vec(), strength: 80, security: Security::Psk }
}

/// Maison-Test (80 %, saved), Bureau-Test (60 %, WPA2); `connected` to the first; `listed`: the
/// detail's lists read.
fn wifi_state(enabled: bool, connected: bool, listed: bool) -> WifiState {
    let aps = vec![home(), AccessPoint { ssid: b"Bureau-Test".to_vec(), strength: 60, security: Security::Psk }];
    WifiState {
        enabled,
        device: Some(WifiDevice {
            state: if connected { 100 } else { 30 },
            active: connected.then(home),
            active_uuid: connected.then(|| HOME.to_string()),
            access_points: listed.then_some(aps),
            saved: listed.then(|| vec![SavedWifi { uuid: HOME.into(), ssid: b"Maison-Test".to_vec() }]),
        }),
        stale: false,
    }
}

fn run_of(parts: &[&str], what: &str) -> QuickAction {
    QuickAction::Run { argv: argv(parts), what: what.into() }
}

#[test]
fn the_wifi_row_in_every_state() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert!(!shown(&l, &q).contains(&Row::Wifi), "nothing read yet: hidden");
    q.set_wifi(Some(WifiState { enabled: true, device: None, stale: false }), t);
    assert!(!shown(&l, &q).contains(&Row::Wifi), "no Wi-Fi device: hidden");
    q.set_wifi(Some(wifi_state(true, true, false)), t);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Wifi, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
    assert_eq!(lines(&l, &q)[2..4], [Line::Separator, Line::Row(Row::Wifi)], "first under the sliders' separator");
    assert_eq!(row_texts(&rows(&l, &q))[3], format!(" {} Wi-Fi{}Maison-Test ›", STRENGTH[3], " ".repeat(12)), "80 %: four steps");
    q.set_wifi(Some(wifi_state(true, false, false)), t);
    assert_eq!(row_texts(&rows(&l, &q))[3], format!(" {WIFI_ON} Wi-Fi{}on ›", " ".repeat(21)));
    q.set_wifi(Some(wifi_state(false, false, false)), t);
    assert_eq!(row_texts(&rows(&l, &q))[3], format!(" {WIFI_OFF} Wi-Fi{}off ›", " ".repeat(20)));
    assert!(describe(&l, &q).starts_with("rows: volume brightness wifi mic"), "{}", describe(&l, &q));
    q.set_wifi(None, t);
    assert!(!shown(&l, &q).contains(&Row::Wifi), "NetworkManager gone: hidden");
}

#[test]
fn space_toggles_the_radio_and_enter_right_or_a_click_open_the_detail() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let rescan = run_of(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_wifi(Some(wifi_state(true, true, true)), t);
    q.selected = Row::Wifi;
    assert_eq!(q.key(&l, Key::Char(' '), false, t), run_of(&["nmcli", "radio", "wifi", "off"], "wifi off"));
    assert_eq!(q.key(&l, Key::Enter, false, t), rescan, "one rescan as it opens");
    assert!(q.wifi.open && lines(&l, &q)[0] == Line::WifiHeader);
    assert_eq!(q.wifi_scope(), WifiScope::Detail);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.wifi.open && q.selected == Row::Wifi, "Escape: back on the Wi-Fi row, the panel open");
    assert_eq!(q.wifi_scope(), WifiScope::Row);
    assert_eq!(q.key(&l, Key::Right, false, t), rescan, "→ opens it too");
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw);
    assert!(!q.wifi.open, "← back");
    q.selected = Row::Volume;
    assert_eq!(q.press(&l, (5, 3), Button::Left, t), rescan, "a click on the Wi-Fi row");
    assert_eq!(q.press(&l, (5, 0), Button::Left, t), QuickAction::Redraw, "a click on the header goes back");
    assert!(!q.wifi.open && q.selected == Row::Wifi);
    q.set_wifi(Some(wifi_state(false, false, false)), t);
    assert_eq!(q.key(&l, Key::Char(' '), false, t), run_of(&["nmcli", "radio", "wifi", "on"], "wifi on"));
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "off: the detail opens, no rescan");
    assert_eq!(row_texts(&rows(&l, &q))[2].trim(), OFF_NOTE);
}

/// Quick Wi-Fi spec § 1: from the network module (or `--quick wifi`), the first Wi-Fi
/// state with a device selects the row and opens the detail, once.
#[test]
fn the_network_module_opens_the_detail_once_the_state_arrives() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let rescan = run_of(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
    let mut q = panel_opened_on(&w, Focus::Wifi);
    assert_eq!(q.selected, Row::Volume, "nothing read yet: the first row");
    assert_eq!(q.set_wifi(Some(wifi_state(true, true, false)), t), WifiNews { action: rescan, failed: None });
    assert!(q.wifi.open && q.selected == Row::Wifi);
    q.key(&l, Key::Escape, false, t);
    assert_eq!(q.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "once only");
    assert!(!q.wifi.open);
    let mut none = panel_opened_on(&w, Focus::Wifi);
    assert_eq!(none.set_wifi(Some(WifiState { enabled: true, device: None, stale: false }), t).action, QuickAction::Redraw);
    assert!(!none.wifi.open && none.selected == Row::Volume, "no device: nothing to open");
    none.set_wifi(Some(wifi_state(true, true, false)), t);
    assert!(none.wifi.open && none.selected == Row::Wifi, "spent only on a state with a device");
    let mut top = panel_opened_on(&w, Focus::Top);
    assert_eq!(top.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "the top: no detail");
}

/// The focus asked at open waits for the user only until they act. Their first key,
/// click or wheel notch drops it: a device appearing later moves nothing and opens no detail.
#[test]
fn the_users_first_key_or_click_drops_a_pending_wifi_focus() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let no_device = || Some(WifiState { enabled: true, device: None, stale: false });
    let mut keyed = panel_opened_on(&w, Focus::Wifi);
    keyed.set_wifi(no_device(), t);
    keyed.key(&l, Key::Down, false, t);
    let chosen = keyed.selected;
    assert_eq!(keyed.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "a key: no rescan");
    assert!(!keyed.wifi.open && keyed.selected == chosen, "a key: the selection stays the user's");
    let mut clicked = panel_opened_on(&w, Focus::Wifi);
    clicked.set_wifi(no_device(), t);
    clicked.press(&l, (5, 1), Button::Left, t);
    let chosen = clicked.selected;
    assert_eq!(clicked.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "a click: no rescan");
    assert!(!clicked.wifi.open && clicked.selected == chosen, "a click: the selection stays the user's");
    let mut wheeled = panel_opened_on(&w, Focus::Wifi);
    wheeled.set_wifi(no_device(), t);
    wheeled.scroll(&l, Some((5, 1)), 1, t);
    wheeled.set_wifi(Some(wifi_state(true, true, false)), t);
    assert!(!wheeled.wifi.open, "a wheel notch: no detail");
    let mut untouched = panel_opened_on(&w, Focus::Wifi);
    untouched.set_wifi(no_device(), t);
    untouched.key(&l, Key::Enter, true, t);
    untouched.set_wifi(Some(wifi_state(true, true, false)), t);
    assert!(untouched.wifi.open, "an auto-repeat ignored by the panel is no act of the user's");
}

#[test]
fn the_window_follows_the_selection_in_the_wifi_detail() {
    let config = QuickConfig { wifi_more: Some(vec!["fake-more".into()]), ..QuickConfig::default() };
    let (w, t) = (World::full(), Instant::now());
    let mut l = w.live();
    l.config = &config;
    let mut s = wifi_state(true, false, true);
    let aps: Vec<AccessPoint> =
        (0..20).map(|n| AccessPoint { ssid: format!("Reseau-Test-{n:02}").into_bytes(), strength: 50, security: Security::Psk }).collect();
    s.device.as_mut().expect("a device").access_points = Some(aps);
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), a_backlight(75), [None, None], 6);
    q.set_wifi(Some(s), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::End, false, t);
    let b = rows(&l, &q);
    assert_eq!(b.rows.len(), 6, "the room under the bar");
    assert_eq!(row_texts(&b).last().map(|s| s.trim().to_string()), Some("More…".to_string()), "More… in view: {:?}", row_texts(&b));
}

/// A Wi-Fi state while open (`follow_wifi`) goes through `fit`; the window
/// follows the selection only when the state moved it (a reorder), never undoing a wheel scroll
/// otherwise; a state that closes the detail (the device gone) brings the window back to the top.
#[test]
fn a_wifi_state_moves_the_window_with_the_selection_and_a_closed_detail_goes_back_to_the_top() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    // Twenty networks at 50 %, Labo-Test (a name short enough to show whole) at `first`:
    // first in the list at 90, last at 10.
    let around = |first: u8| {
        let aps = (0..20)
            .map(|n| AccessPoint {
                ssid: if n == 0 { b"Labo-Test".to_vec() } else { format!("Reseau-Test-{n:02}").into_bytes() },
                strength: if n == 0 { first } else { 50 },
                security: Security::Psk,
            })
            .collect();
        let device = WifiDevice { state: 30, active: None, active_uuid: None, access_points: Some(aps), saved: Some(vec![]) };
        WifiState { enabled: true, device: Some(device), stale: false }
    };
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), a_backlight(75), [None, None], 6);
    q.follow_wifi(&l, Some(around(90)), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Home, false, t);
    assert_eq!(q.top, 0);
    let picked = |q: &Quick| row_texts(&rows(&l, q)).iter().any(|r| r.contains("Labo-Test"));
    assert!(picked(&q), "first: in view");
    q.follow_wifi(&l, Some(around(10)), t);
    assert!(q.top > 0 && picked(&q), "moved to the end by the state: the window follows it ({})", q.top);
    q.scroll(&l, None, -3, t);
    let scrolled = q.top;
    assert!(!picked(&q), "scrolled away from the selection");
    q.follow_wifi(&l, Some(around(10)), t);
    assert_eq!(q.top, scrolled, "a state that moves nothing keeps the wheel's window");
    q.follow_wifi(&l, Some(WifiState { enabled: true, device: None, stale: false }), t);
    assert!(!q.wifi.open, "the device gone: the detail closed");
    assert_eq!(q.top, 0, "back to the top");
}

/// The panel's one timer covers the connect timeout and the `failed` display (quick Wi-Fi § 5).
#[test]
fn the_wifi_attempt_rides_the_panel_s_one_timer() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_wifi(Some(wifi_state(true, true, true)), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "Bureau-Test: the field");
    for c in "fake-pass".chars() {
        q.key(&l, Key::Char(c), false, t);
    }
    assert!(matches!(q.key(&l, Key::Enter, false, t), QuickAction::RunSecret { .. }));
    assert_eq!(q.deadline(), Some(t + crate::quick::wifi::CONNECT_TIMEOUT));
    let d = q.due(t + crate::quick::wifi::CONNECT_TIMEOUT);
    assert_eq!((d.wifi_failed, d.redraw), (Some("no answer in 20 s"), true));
    assert_eq!(q.deadline(), Some(t + crate::quick::wifi::CONNECT_TIMEOUT + crate::quick::wifi::FAILED_SHOWN));
}

/// A held Backspace keeps erasing in the password field; elsewhere it
/// does not repeat, nor does a held character.
#[test]
fn backspace_repeats_in_the_password_field_only() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_wifi(Some(wifi_state(true, true, true)), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Down, false, t);
    q.key(&l, Key::Enter, false, t);
    for c in "abc".chars() {
        q.key(&l, Key::Char(c), false, t);
    }
    assert_eq!(q.key(&l, Key::Char('d'), true, t), QuickAction::Nothing, "a held character: once");
    assert_eq!(q.key(&l, Key::Backspace, true, t), QuickAction::Redraw, "a held Backspace erases");
    assert_eq!(q.wifi.password().map(|s| s.bytes()), Some(&b"ab"[..]));
    q.key(&l, Key::Escape, false, t);
    assert_eq!(q.key(&l, Key::Backspace, true, t), QuickAction::Nothing, "no field: no repeat");
}

fn casque(active: HeadsetMode) -> Option<Headset> {
    Some(Headset { device: 61, label: "CasqueBT".into(), active, hifi: 1, mic: 3 })
}

#[test]
fn the_headset_row_switches_to_the_other_kind() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Sound);
    q.set_headset(casque(HeadsetMode::HiFi));
    q.selected = Row::Headset;
    let to_mic = QuickAction::Run { argv: argv(&["wpctl", "set-profile", "61", "3"]), what: "headset mode mic".into() };
    for k in [Key::Enter, Key::Char(' '), Key::Left, Key::Right] {
        assert_eq!(q.clone().key(&l, k, false, t), to_mic, "{k:?}");
    }
    let y = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Headset)).expect("shown") as u16;
    assert_eq!(q.clone().press(&l, (10, y), Button::Left, t), to_mic, "a click is Enter");
    q.set_headset(casque(HeadsetMode::Mic));
    let to_hifi = QuickAction::Run { argv: argv(&["wpctl", "set-profile", "61", "1"]), what: "headset mode hi-fi".into() };
    assert_eq!(q.clone().key(&l, Key::Right, false, t), to_hifi);
    assert_eq!(q.key(&l, Key::Right, true, t), QuickAction::Nothing, "a held arrow switches once");
}

#[test]
fn the_headset_row_shows_under_the_volume_only_with_a_headset() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert!(!shown(&l, &q).contains(&Row::Headset));
    q.set_headset(casque(HeadsetMode::HiFi));
    assert_eq!(shown(&l, &q)[..3], [Row::Volume, Row::Headset, Row::Brightness]);
    assert_eq!(lines(&l, &q)[..4], [Line::Row(Row::Volume), Line::Row(Row::Headset), Line::Row(Row::Brightness), Line::Separator]);
    let t = row_texts(&rows(&l, &q));
    assert!(t[1].contains("CasqueBT") && t[1].ends_with("‹ Hi-Fi ›"), "{:?}", t[1]);
    q.set_headset(casque(HeadsetMode::Mic));
    assert!(row_texts(&rows(&l, &q))[1].ends_with("‹ Mic ›"));
    assert!(describe(&l, &q).contains("rows: volume headset brightness"));
    q.selected = Row::Headset;
    q.set_headset(None);
    q.fit(&l);
    assert_eq!(q.selected, Row::Brightness, "its row gone: the next one below");
}

/// Every variant with the row it must have. The `match` is exhaustive: a variant added to `Row`
/// stops this file compiling until it is listed here, and listed, its table row must exist.
macro_rules! rows {
    ($($variant:ident => $index:expr),* $(,)?) => {
        #[allow(dead_code)]
        fn row_of(r: Row) -> usize {
            match r {
                $(Row::$variant => $index),*
            }
        }
        const LISTED: &[(Row, usize)] = &[$((Row::$variant, $index)),*];
    };
}

rows! {
    Volume => 0, Headset => 1, Brightness => 2, Wifi => 3, Bluetooth => 4, Vpn => 5, Shield => 6, Mic => 7, Mixer => 8,
    Dnd => 9, Profile => 10, KeepAwake => 11, NightLight => 12, Session => 13,
}

/// `ROWS` is indexed by `Row as usize`: one entry per variant, in variant order.
#[test]
fn one_spec_per_row_in_variant_order() {
    for &(r, i) in LISTED {
        assert_eq!(r as usize, i, "{r:?}'s discriminant is not its index");
        assert_eq!(ROWS.get(i).map(|s| s.row), Some(r), "{r:?} has no entry at {i}");
    }
    assert_eq!(LISTED.len(), ROWS.len(), "an entry for no variant, or a variant for no entry");
    let names: Vec<&str> = ROWS.iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        [
            "volume",
            "headset",
            "brightness",
            "wifi",
            "bluetooth",
            "vpn",
            "shield",
            "mic",
            "mixer",
            "dnd",
            "profile",
            "keep-awake",
            "night-light",
            "session"
        ]
    );
    let groups: Vec<u8> = ROWS.iter().map(|s| s.group).collect();
    assert_eq!(groups, [0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2]);
}

/// A re-read pending on any one toggle is a deadline (the timer must be armed for it).
#[test]
fn a_pending_toggle_reread_is_the_deadline_whichever_toggle() {
    let w = World::full();
    let at = Instant::now() + Duration::from_millis(300);
    for t in Toggle::ALL {
        let mut q = panel_opened_on(&w, Focus::Top);
        assert_eq!(q.deadline(), None, "{t:?}: nothing pending");
        q.reread_toggles[t.index()] = Some(at);
        assert_eq!(q.deadline(), Some(at), "{t:?}");
    }
}

/// `[quick] mixer`: a `Mixer…` row right after the microphone, shown only with a command (an
/// unresolved config, a test's default, or `[]` hides it), run as given and then the panel closes.
#[test]
fn the_mixer_row_runs_its_command_then_closes() {
    let command = vec!["fake-term".to_string(), "-e".into(), "fake-mixer".into()];
    let mut w = World::full();
    let t = Instant::now();
    assert!(!shown(&w.live(), &panel_opened_on(&w, Focus::Top)).contains(&Row::Mixer), "unresolved: no row");
    w.config.mixer = Some(vec![]);
    assert!(!shown(&w.live(), &panel_opened_on(&w, Focus::Top)).contains(&Row::Mixer), "[] hides it");
    w.config.mixer = Some(command.clone());
    let l = w.live();
    let q = panel_opened_on(&w, Focus::Top);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Mic, Row::Mixer, Row::Dnd, Row::Profile, Row::Session]);
    let b = rows(&l, &q);
    assert_eq!(row_texts(&b)[4], format!(" {MIXER} Mixer…"), "{:?}", row_texts(&b));
    assert_eq!(b.separators, [2, 7], "in the microphone's group");
    let run = QuickAction::RunAndClose { argv: command, what: "mixer".into() };
    for k in [Key::Enter, Key::Char(' ')] {
        let mut q = panel_opened_on(&w, Focus::Top);
        q.selected = Row::Mixer;
        assert_eq!(q.key(&l, k, false, t), run, "{k:?}");
    }
    let mut q = panel_opened_on(&w, Focus::Top);
    assert_eq!(q.press(&l, (5, 4), Button::Left, t), run, "a left click");
    assert_eq!(q.selected, Row::Mixer);
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Mixer;
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Nothing, "arrows do nothing");
}

#[test]
fn enter_and_right_open_the_dnd_detail_and_space_still_toggles() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    let t = Instant::now();
    q.selected = Row::Dnd;
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::ToggleDnd);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert!(q.dnd.open);
    assert_eq!(q.dnd.selected, 1, "off: opens on 30 minutes");
    let b = rows(&l, &q);
    let t_ = row_texts(&b);
    assert!(t_[0].trim_start().starts_with("‹ Do not disturb"), "{:?}", t_[0]);
    assert_eq!(b.separators, [1]);
    let labels = ["Off", "30 minutes", "1 hour", "2 hours", "Until 08:00", "Until I turn it off"];
    for (i, label) in labels.iter().enumerate() {
        let mark = if i == 0 { "●" } else { " " };
        assert!(t_[2 + i].starts_with(&format!(" {mark} ")) && t_[2 + i].ends_with(label), "line {i}: {:?}", t_[2 + i]);
    }
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.dnd.open);
    assert_eq!(q.selected, Row::Dnd);
    assert_eq!(q.key(&l, Key::Right, false, t), QuickAction::Redraw);
    assert!(q.dnd.open, "→ opens it again");
}

#[test]
fn a_do_not_disturb_choice_applies_and_goes_back() {
    let w = World::full();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    let t = Instant::now();
    q.selected = Row::Dnd;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Dnd(DndChoice::For(3600)));
    assert!(!q.dnd.open);
    assert_eq!(q.selected, Row::Dnd);
    q.key(&l, Key::Enter, false, t);
    // Lines: 0 header, 1 separator, 2.. the choices; Untimed is the last of the six.
    assert_eq!(q.press(&l, (10, 7), Button::Left, t), QuickAction::Dnd(DndChoice::Untimed));
    assert!(!q.dnd.open);
    q.key(&l, Key::Enter, false, t);
    assert_eq!(q.press(&l, (10, 0), Button::Left, t), QuickAction::Redraw, "the header goes back");
    assert!(!q.dnd.open);
    assert_eq!(q.selected, Row::Dnd);
    q.key(&l, Key::Enter, false, t);
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Nothing, "Space does nothing in the detail");
    assert_eq!(q.press(&l, (10, 7), Button::Right, t), QuickAction::Nothing, "nor the right button");
    assert!(q.dnd.open);
}

#[test]
fn a_press_on_the_switch_opens_the_detail() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert_eq!(q.press(&l, (10, DND_LINE), Button::Left, t), QuickAction::ToggleDnd);
    assert!(!q.dnd.open);
    assert_eq!(q.press(&l, (27, DND_LINE), Button::Left, t), QuickAction::Redraw);
    assert!(q.dnd.open);
    let mut q = panel_opened_on(&w, Focus::Top);
    assert_eq!(q.press(&l, (25, DND_LINE), Button::Left, t), QuickAction::ToggleDnd, "just left of the switch");
    for col in [26, 33] {
        let mut q = panel_opened_on(&w, Focus::Top);
        assert_eq!(q.press(&l, (col, DND_LINE), Button::Left, t), QuickAction::Redraw, "column {col} is the switch's");
        assert!(q.dnd.open);
    }
    // The cells drawn are the cells that hit: the switch's pill starts where the press opens.
    let b = rows(&l, &q);
    assert_eq!(b.rows[DND_LINE as usize].styles().find('s'), Some(26));
}

#[test]
fn the_marks_follow_the_state_while_open() {
    let mut w = World::full();
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Dnd;
    q.key(&w.live(), Key::Enter, false, t);
    w.dnd = Some(true);
    let b = rows(&w.live(), &q);
    let t_ = row_texts(&b);
    assert!(t_[7].contains("● ") && t_[7].ends_with("Until I turn it off"), "{:?}", t_[7]);
    assert!(!t_[2].contains('●'), "{:?}", t_[2]);
    assert!(q.dnd.open, "still open");
    q.fit(&w.live());
    assert!(q.dnd.open);
    // A timed end at the configured time marks `Until`.
    w.dnd_end = Some((8, 0));
    let timed = row_texts(&rows(&w.live(), &q));
    assert!(timed[6].contains("● ") && timed[6].ends_with("Until 08:00"), "{:?}", timed[6]);
    assert!(!timed[7].contains('●'), "{:?}", timed[7]);
    // The row going (no notifications module) closes it.
    w.dnd = None;
    q.fit(&w.live());
    assert!(!q.dnd.open);
    assert_ne!(q.selected, Row::Dnd, "the row is gone: the selection fell back");
}

#[test]
fn left_goes_back_from_the_do_not_disturb_detail_and_its_header_shows_a_timed_end() {
    let mut w = World::full();
    let t = Instant::now();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.selected = Row::Dnd;
    q.key(&w.live(), Key::Enter, false, t);
    assert!(!row_texts(&rows(&w.live(), &q))[0].contains("until"), "untimed: no end on the header");
    w.dnd = Some(true);
    w.dnd_end = Some((7, 5));
    let header = row_texts(&rows(&w.live(), &q))[0].clone();
    assert!(header.ends_with("until 07:05"), "{header:?}");
    assert_eq!(q.key(&w.live(), Key::Left, false, t), QuickAction::Redraw);
    assert!(!q.dnd.open);
    assert_eq!(q.selected, Row::Dnd);
}

#[test]
fn a_timed_dnd_shows_its_end_in_the_switch() {
    let mut w = World::full();
    let t = Instant::now();
    w.dnd = Some(true);
    w.dnd_end = Some((14, 30));
    let q = panel_opened_on(&w, Focus::Top);
    let b = rows(&w.live(), &q);
    let row = b.rows[DND_LINE as usize].text();
    assert!(row.ends_with(" until 14:30 ●  "), "{row:?}");
    assert_eq!(b.rows[DND_LINE as usize].styles()[18..33], *"AAAAAAAAAAAAAAA");
    // The press area is the drawn pill: its first cell opens the detail, the cell before toggles.
    let mut q2 = panel_opened_on(&w, Focus::Top);
    assert_eq!(q2.press(&w.live(), (19, DND_LINE), Button::Left, t), QuickAction::Redraw);
    assert!(q2.dnd.open);
    let mut q3 = panel_opened_on(&w, Focus::Top);
    assert_eq!(q3.press(&w.live(), (17, DND_LINE), Button::Left, t), QuickAction::ToggleDnd);
    assert!(!q3.dnd.open);
    let header = row_texts(&rows(&w.live(), &q2))[0].clone();
    assert!(header.trim_start().starts_with("‹ Do not disturb") && header.ends_with("until 14:30"), "{header:?}");
    w.dnd_end = None;
    let b = rows(&w.live(), &q);
    assert!(b.rows[DND_LINE as usize].text().ends_with(" ON  ●  "));
}

#[test]
fn live_works_out_the_local_end_of_a_timed_dnd() {
    use crate::notify::dnd::Dnd;
    let bar = Bar::new(crate::config::BarConfig::default());
    let until = 1_800_000_000u64;
    let lt = crate::clock::LocalTime::from_epoch(until as i64);
    let timed = Dnd { on: true, until: Some(until), by_sharing: false };
    let l = live(&bar, &timed);
    assert_eq!(l.dnd, Some(true));
    assert_eq!(l.dnd_end, Some((lt.hour, lt.minute)));
    assert_eq!(live(&bar, &Dnd { on: false, ..timed }).dnd_end, None, "off: no end");
    assert_eq!(live(&bar, &Dnd { by_sharing: true, ..timed }).dnd_end, None, "sharing: no end");
    assert_eq!(live(&bar, &Dnd::user(true)).dnd_end, None, "untimed: no end");
}

fn vpn_uuid(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// VPN-Test-1 and VPN-Test-2 (plugin) and WG-Test (WireGuard), fictional; `active`: (profile, state).
fn vpn_state(count: usize, active: &[(usize, u32)]) -> VpnState {
    let names = ["VPN-Test-1", "VPN-Test-2", "WG-Test"];
    VpnState {
        profiles: (1..=count)
            .map(|n| VpnProfile {
                uuid: vpn_uuid(n),
                name: names[n - 1].into(),
                kind: if n == 3 { VpnKind::WireGuard } else { VpnKind::Plugin },
                last_used: n as u64 * 100,
                asks: false,
            })
            .collect(),
        active: active.iter().map(|(n, state)| ActiveVpn { uuid: vpn_uuid(*n), state: *state }).collect(),
    }
}

fn vpn_config() -> QuickConfig {
    QuickConfig { vpn_more: Some(vec!["fake-vpn-more".into()]), ..QuickConfig::default() }
}

#[test]
fn the_vpn_row_sits_under_bluetooth_and_hides_without_profiles() {
    let (mut w, t) = (World::full(), Instant::now());
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    assert!(!shown(&l, &q).contains(&Row::Vpn), "nothing read yet: hidden");
    q.set_vpn(Some(VpnState { profiles: vec![], active: vec![] }), t);
    assert!(!shown(&l, &q).contains(&Row::Vpn), "no profile: hidden");
    q.set_vpn(Some(vpn_state(2, &[(2, 2)])), t);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Bluetooth, Row::Vpn, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
    let at = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Vpn)).expect("the row");
    assert_eq!(lines(&l, &q)[at - 1], Line::Row(Row::Bluetooth), "right under Bluetooth, one block with it");
    assert_eq!(row_texts(&rows(&l, &q))[at], format!(" {} VPN{}VPN-Test-2 ›", crate::segments::VPN, " ".repeat(15)));
    assert!(describe(&l, &q).starts_with("rows: volume brightness bluetooth vpn mic"), "{}", describe(&l, &q));
    q.set_vpn(Some(vpn_state(2, &[])), t);
    assert_eq!(row_texts(&rows(&l, &q))[at], format!(" {} VPN{}off ›", crate::segments::VPN, " ".repeat(22)));
    q.set_vpn(None, t);
    assert!(!shown(&l, &q).contains(&Row::Vpn), "NetworkManager gone: hidden");
}

#[test]
fn space_on_the_vpn_row_toggles() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_vpn(Some(vpn_state(3, &[])), t);
    q.selected = Row::Vpn;
    // The last used (WG-Test, 300) comes up; an attempt then blocks a second Space.
    assert_eq!(q.key(&l, Key::Char(' '), false, t), run(&["nmcli", "connection", "up", "uuid", &vpn_uuid(3)], "vpn up"));
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Nothing);
    q.set_vpn(Some(vpn_state(3, &[(3, 2)])), t);
    assert_eq!(q.key(&l, Key::Char(' '), false, t), run(&["nmcli", "connection", "down", "uuid", &vpn_uuid(3)], "vpn down"));
    assert_eq!(q.key(&l, Key::Char(' '), true, t), QuickAction::Nothing, "a held Space acts once");
}

#[test]
fn enter_on_the_vpn_row_opens_the_detail_and_escape_comes_back() {
    let (mut w, t) = (World::full(), Instant::now());
    w.config = vpn_config();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_vpn(Some(vpn_state(2, &[])), t);
    q.selected = Row::Vpn;
    assert_eq!(q.vpn_scope(), VpnScope::Row);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert!(q.vpn.open);
    assert_eq!(q.vpn_scope(), VpnScope::Detail);
    assert_eq!(lines(&l, &q)[..3], [Line::VpnHeader, Line::Separator, Line::VpnProfile(0)]);
    assert_eq!(row_texts(&rows(&l, &q))[0], " ‹ VPN");
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.vpn.open && q.selected == Row::Vpn, "back on the VPN row, the panel open");
    assert_eq!(q.key(&l, Key::Right, false, t), QuickAction::Redraw, "→ opens it too");
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw);
    assert!(!q.vpn.open);
    // A click on the row, then on the header.
    q.selected = Row::Volume;
    let at = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Vpn)).expect("the row") as u16;
    assert_eq!(q.press(&l, (5, at), Button::Left, t), QuickAction::Redraw);
    assert!(q.vpn.open && q.selected == Row::Vpn);
    assert_eq!(q.press(&l, (5, 0), Button::Left, t), QuickAction::Redraw);
    assert!(!q.vpn.open && q.selected == Row::Vpn);
    // A row of the detail acts.
    q.key(&l, Key::Enter, false, t);
    assert_eq!(
        q.key(&l, Key::Enter, false, t),
        run(&["nmcli", "connection", "up", "uuid", &vpn_uuid(1)], "vpn up"),
        "Enter on the first profile"
    );
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing, "a repeat Enter runs nothing");
}

/// No key path reaches the VPN row from another detail, so `open_vpn` is asked directly: it takes
/// the panel's one detail, as `open_bt` does.
#[test]
fn opening_the_vpn_closes_the_other_details() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_wifi(Some(wifi_state(true, true, true)), t);
    q.set_vpn(Some(vpn_state(2, &[])), t);
    q.wifi.open_detail();
    q.detail = true;
    q.top = 3;
    assert_eq!(q.open_vpn(&l), QuickAction::Redraw);
    assert!(q.vpn.open && !q.detail && q.top == 0 && !q.wifi.open);
    let mut none = panel_opened_on(&w, Focus::Top);
    assert_eq!(none.open_vpn(&l), QuickAction::Nothing, "no VPN row: nothing opens");
    assert!(!none.vpn.open);
}

#[test]
fn a_held_enter_on_the_vpn_row_runs_nothing() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_vpn(Some(vpn_state(2, &[])), t);
    q.selected = Row::Vpn;
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing, "a held Enter does not even open it");
    assert!(!q.vpn.open);
    q.key(&l, Key::Enter, false, t);
    assert!(matches!(q.key(&l, Key::Enter, false, t), QuickAction::Run { .. }));
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Nothing, "one attempt at a time");
}

/// From `--quick vpn`, the first VPN state with a profile selects the row and opens the detail, once;
/// the user's first key drops the wish.
#[test]
fn the_vpn_focus_opens_the_detail_on_the_first_state_with_a_profile() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Vpn);
    assert_eq!(q.selected, Row::Volume, "nothing read yet: the first row");
    assert_eq!(q.set_vpn(Some(VpnState { profiles: vec![], active: vec![] }), t).action, QuickAction::Redraw);
    assert!(!q.vpn.open, "no profile: kept");
    let news = q.set_vpn(Some(vpn_state(2, &[])), t);
    assert_eq!(news, WifiNews { action: QuickAction::Redraw, failed: None });
    assert!(q.vpn.open && q.selected == Row::Vpn);
    q.key(&l, Key::Escape, false, t);
    q.set_vpn(Some(vpn_state(2, &[])), t);
    assert!(!q.vpn.open, "once only");
    let mut top = panel_opened_on(&w, Focus::Top);
    top.set_vpn(Some(vpn_state(2, &[])), t);
    assert!(!top.vpn.open, "the top: no detail");
    let mut keyed = panel_opened_on(&w, Focus::Vpn);
    keyed.key(&l, Key::Down, false, t);
    keyed.set_vpn(Some(vpn_state(2, &[])), t);
    assert!(!keyed.vpn.open, "a key drops it");
    let mut clicked = panel_opened_on(&w, Focus::Vpn);
    clicked.press(&l, (5, 1), Button::Left, t);
    clicked.set_vpn(Some(vpn_state(2, &[])), t);
    assert!(!clicked.vpn.open, "a click drops it");
    let mut wheeled = panel_opened_on(&w, Focus::Vpn);
    wheeled.scroll(&l, Some((5, 1)), 1, t);
    wheeled.set_vpn(Some(vpn_state(2, &[])), t);
    assert!(!wheeled.vpn.open, "a wheel notch drops it");
    // follow_vpn: the same, then fitted and the window on the selection.
    let mut f = panel_opened_on(&w, Focus::Vpn);
    f.follow_vpn(&l, Some(vpn_state(2, &[])), t);
    assert!(f.vpn.open && f.selected == Row::Vpn);
}

#[test]
fn the_last_profile_gone_closes_the_detail() {
    let (mut w, t) = (World::full(), Instant::now());
    w.config = vpn_config();
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Vpn);
    q.follow_vpn(&l, Some(vpn_state(3, &[])), t);
    assert!(q.vpn.open);
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.vpn.pick, crate::quick::vpn::Pick::Profile(vpn_uuid(2)));
    // One of three gone: the pick moves to its neighbour, the detail stays.
    let mut s = vpn_state(3, &[]);
    s.profiles.retain(|p| p.uuid != vpn_uuid(2));
    q.follow_vpn(&l, Some(s), t);
    assert!(q.vpn.open);
    assert_eq!(q.vpn.pick, crate::quick::vpn::Pick::Profile(vpn_uuid(3)));
    // All gone: back on the rows, the row hidden, the selection on a row still shown.
    q.top = 4;
    q.follow_vpn(&l, Some(VpnState { profiles: vec![], active: vec![] }), t);
    assert!(!q.vpn.open);
    assert!(!shown(&l, &q).contains(&Row::Vpn));
    assert!(shown(&l, &q).contains(&q.selected), "{:?}", q.selected);
    assert_eq!(q.top, 0);
    // NetworkManager gone closes it too.
    let mut g = panel_opened_on(&w, Focus::Vpn);
    g.follow_vpn(&l, Some(vpn_state(2, &[])), t);
    assert!(g.vpn.open);
    g.follow_vpn(&l, None, t);
    assert!(!g.vpn.open && !shown(&l, &g).contains(&Row::Vpn));
}

#[test]
fn the_vpn_rides_the_panel_s_one_timer() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_vpn(Some(vpn_state(2, &[])), t);
    assert_eq!(q.deadline(), None);
    q.selected = Row::Vpn;
    q.key(&l, Key::Char(' '), false, t);
    assert_eq!(q.vpn_scope(), VpnScope::Detail, "an attempt widens the read");
    assert_eq!(q.deadline(), Some(t + crate::quick::vpn::CONNECT_TIMEOUT));
    assert_eq!(q.due(t + Duration::from_secs(19)).vpn_failed, None);
    let due = q.due(t + crate::quick::vpn::CONNECT_TIMEOUT);
    assert_eq!(due.vpn_failed, Some("no answer in 20 s"));
    assert!(due.redraw);
    assert_eq!(q.deadline(), Some(t + crate::quick::vpn::CONNECT_TIMEOUT + FAILED_SHOWN), "the failed mark");
    assert!(q.due(t + crate::quick::vpn::CONNECT_TIMEOUT + FAILED_SHOWN).redraw);
    assert_eq!(q.deadline(), None);
}

#[test]
fn a_vpn_state_attempt_failure_reaches_the_news() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Top);
    q.set_vpn(Some(vpn_state(2, &[])), t);
    q.selected = Row::Vpn;
    q.key(&l, Key::Char(' '), false, t);
    q.follow_vpn(&l, Some(vpn_state(2, &[(2, 1)])), t);
    let news = q.follow_vpn(&l, Some(vpn_state(2, &[])), t);
    assert_eq!(news.failed, Some("the activation failed"));
}

#[test]
fn the_window_follows_the_selection_in_the_vpn_detail() {
    let (mut w, t) = (World::full(), Instant::now());
    w.config = vpn_config();
    let l = w.live();
    let mut s = vpn_state(2, &[]);
    s.profiles = (0..16)
        .map(|n| VpnProfile { uuid: vpn_uuid(n), name: format!("VPN-Test-{n:02}"), kind: VpnKind::Plugin, last_used: 0, asks: false })
        .collect();
    let mut q = Quick::new(Focus::Vpn, &l, two_sinks(), a_backlight(75), [None, None], 6);
    q.follow_vpn(&l, Some(s), t);
    assert!(q.vpn.open && q.top == 0);
    for _ in 0..10 {
        q.key(&l, Key::Down, false, t);
    }
    let all = lines(&l, &q);
    let at = q.selected_line(&all).expect("a selected line");
    assert_eq!(all[at], Line::VpnProfile(10));
    assert!((q.top..q.top + 6).contains(&at), "the selection stays in view: top {} at {at}", q.top);
    q.key(&l, Key::End, false, t);
    let all = lines(&l, &q);
    assert_eq!(all[q.selected_line(&all).expect("selected")], Line::VpnMore);
    assert_eq!(q.top, all.len() - 6, "More… at the bottom of the window");
    assert_eq!(row_texts(&rows(&l, &q)).last().map(|s| s.trim()), Some("More…"));
}

#[test]
fn a_click_on_the_shield_module_asks_for_the_shield_focus() {
    assert_eq!(Focus::of(&Target::Module(ModuleId::Shield)), Focus::Shield);
}

// ---- the Shield row (shield spec § 5) ----

fn shield_view(state: Option<bool>, can_list: bool, can_allow: bool) -> crate::shield::ShieldView {
    crate::shield::ShieldView { state, switching: false, failed_until: None, can_list, can_allow }
}

fn shield_world(view: crate::shield::ShieldView) -> World {
    let mut w = World::full();
    w.shield = Some(view);
    w
}

#[test]
fn the_shield_row_sits_under_vpn_and_needs_shield() {
    let none = World::full();
    assert!(!shown(&none.live(), &panel_opened_on(&none, Focus::Top)).contains(&Row::Shield), "no [shield]: no row");
    let w = shield_world(shield_view(Some(false), true, true));
    let (l, q) = (w.live(), panel_opened_on(&w, Focus::Top));
    let all = shown(&l, &q);
    let at = all.iter().position(|r| *r == Row::Shield).expect("the row");
    assert_eq!(all[at + 1], Row::Mic);
    assert_eq!(ROWS[Row::Shield as usize].group, 1);
    assert_eq!(Row::Shield as usize, Row::Vpn as usize + 1, "right under the VPN row");
    assert_eq!(panel_opened_on(&w, Focus::Shield).selected, Row::Shield, "a click on the glyph selects the row");
    assert!(!panel_opened_on(&w, Focus::Shield).shield.open, "and opens nothing");
    assert_eq!(panel_opened_on(&none, Focus::Shield).selected, Row::Volume, "no row: the first one");
}

#[test]
fn the_shield_row_draws_its_switch() {
    let text = |v: crate::shield::ShieldView| {
        let w = shield_world(v);
        let (l, q) = (w.live(), panel_opened_on(&w, Focus::Shield));
        let at = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Shield)).unwrap();
        row_texts(&rows(&l, &q))[at].clone()
    };
    let glyph = crate::segments::SHIELD;
    assert_eq!(text(shield_view(Some(true), true, true)), format!(" {glyph} Shield{} ON  ●", " ".repeat(17)));
    assert_eq!(text(shield_view(Some(false), true, true)), format!(" {glyph} Shield{} ○ off", " ".repeat(17)));
    assert!(text(shield_view(None, true, true)).ends_with(" ·"));
    let mut v = shield_view(Some(false), true, true);
    v.switching = true;
    assert!(text(v).ends_with(" …"), "{}", text(v));
    let mut v = shield_view(Some(false), true, true);
    v.failed_until = Some(Instant::now() + Duration::from_secs(4));
    assert!(text(v).ends_with(" failed"), "{}", text(v));
}

#[test]
fn space_on_the_shield_row_toggles_unless_held_switching_or_unknown() {
    let t = Instant::now();
    let w = shield_world(shield_view(Some(false), true, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Shield(ShieldAsk::Toggle));
    assert_eq!(q.key(&l, Key::Char(' '), true, t), QuickAction::Nothing, "a held Space starts one command");
    let mut v = shield_view(Some(false), true, true);
    v.switching = true;
    let w = shield_world(v);
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Nothing, "while a command runs");
    let w = shield_world(shield_view(None, true, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Nothing, "unknown state");
}

#[test]
fn enter_on_the_shield_row_opens_its_detail_unless_blocked_is_empty() {
    let t = Instant::now();
    for key in [Key::Enter, Key::Right] {
        let w = shield_world(shield_view(Some(true), true, true));
        let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
        assert_eq!(q.key(&l, key, false, t), QuickAction::Redraw);
        assert!(q.shield.open && q.shield.read_wanted, "{key:?}");
        assert_eq!(lines(&l, &q)[0], Line::ShieldHeader);
    }
    let w = shield_world(shield_view(Some(true), false, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Nothing);
    assert!(!q.shield.open);
    // A left click on the row is Enter.
    let w = shield_world(shield_view(Some(true), true, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    let at = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Shield)).unwrap() as u16;
    q.press(&l, (5, at), Button::Left, t);
    assert!(q.shield.open);
}

#[test]
fn escape_in_the_shield_detail_comes_back_to_the_shield_row() {
    let t = Instant::now();
    let w = shield_world(shield_view(Some(true), true, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    q.key(&l, Key::Enter, false, t);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.shield.open && q.selected == Row::Shield, "back on the row, the panel open");
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Close, "a second Esc closes the panel");
    // The row going (the config reloaded away, say) closes the detail under it.
    q.key(&l, Key::Enter, false, t);
    let none = World::full();
    q.fit(&none.live());
    assert!(!q.shield.open);
}

#[test]
fn a_device_row_is_reached_through_the_panels_keys() {
    let t = Instant::now();
    let w = shield_world(shield_view(Some(true), true, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    q.key(&l, Key::Enter, false, t);
    q.shield.set_listing(
        &l.shield.unwrap(),
        Ok(vec![crate::shield::usb::UsbDevice { id: 7, vid_pid: "1234:5678".into(), name: "Test Stick".into() }]),
    );
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Shield(ShieldAsk::Allow(7)));
    // A repeat of Enter allows nothing.
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing);
}

#[test]
fn the_shield_rows_failed_tag_ends_at_its_deadline_with_a_redraw() {
    let t = Instant::now();
    let mut v = shield_view(Some(false), true, true);
    v.failed_until = Some(t + Duration::from_secs(4));
    let w = shield_world(v);
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    q.fit(&l);
    assert_eq!(q.deadline(), Some(t + Duration::from_secs(4)));
    let d = q.due(t + Duration::from_secs(1));
    assert!(!d.redraw && !d.shield_failed);
    let d = q.due(t + Duration::from_secs(4));
    assert!(d.redraw && d.shield_failed, "the wiring clears the model's `failed`");
    assert_eq!(q.deadline(), None);
}

#[test]
fn open_shield_selects_the_row_and_opens_its_detail() {
    // What `App::shield_open_detail` does inside the panel: the row selected, its detail open and
    // one read asked.
    let w = shield_world(shield_view(Some(true), true, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    assert_eq!(q.selected, Row::Shield);
    assert_eq!(q.open_shield(&l), QuickAction::Redraw);
    assert!(q.shield.open && q.shield.read_wanted);
    assert_eq!(lines(&l, &q)[0], Line::ShieldHeader);
    // Without a listable `blocked` nothing opens.
    let w = shield_world(shield_view(Some(true), false, true));
    let (l, mut q) = (w.live(), panel_opened_on(&w, Focus::Shield));
    assert_eq!(q.open_shield(&l), QuickAction::Nothing);
    assert!(!q.shield.open);
}

#[test]
fn opening_the_shield_detail_closes_the_vpn_and_do_not_disturb_ones_in_place() {
    let w = shield_world(shield_view(Some(true), true, true));
    let l = w.live();
    let mut q = panel_opened_on(&w, Focus::Shield);
    assert!(!q.other_detail_open());
    q.vpn.open = true;
    assert!(!q.other_detail_open(), "the VPN detail closes in place: no panel re-open");
    q.open_shield(&l);
    assert!(q.shield.open && !q.vpn.open, "the VPN detail closed in place");
    q.dnd.open = true;
    q.open_shield(&l);
    assert!(!q.dnd.open, "and the do-not-disturb one");
    assert!(!q.other_detail_open());
    // Orders-carrying details (Bluetooth) and Wi-Fi are the wiring's: it sees them and re-opens the panel.
    q.bt.open = true;
    assert!(q.other_detail_open());
    q.bt.open = false;
    q.wifi.open = true;
    assert!(q.other_detail_open());
}
