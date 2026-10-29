use super::super::testing::*;
use super::super::worker::{Op, Request};
use super::super::{Confirm, SliderId, ToggleId};
use super::*;
use crate::backends::{NodeRef, PowerAction};
use ratatui::crossterm::event::KeyEventState;

fn press(a: &mut App, code: KeyCode, mods: KeyModifiers) {
    a.on_key(KeyEvent::new(code, mods), Instant::now());
}
fn k(a: &mut App, code: KeyCode) {
    press(a, code, KeyModifiers::NONE);
}
fn c(a: &mut App, ch: char) {
    k(a, KeyCode::Char(ch));
}
fn typed(a: &mut App, s: &str) {
    s.chars().for_each(|ch| c(a, ch));
}

#[test]
fn quit() {
    let mut a = ready_app();
    c(&mut a, 'q');
    assert!(a.should_quit);
}

#[test]
fn tabs() {
    let mut a = ready_app();
    c(&mut a, '2');
    assert_eq!(a.tab, Tab::Sound);
    k(&mut a, KeyCode::Tab);
    assert_eq!(a.tab, Tab::Network);
    press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
    press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
    press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(a.tab, Tab::Appearance, "Shift+Tab from Home: the last tab");
    press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(a.tab, Tab::Displays, "on Appearance too, Shift+Tab changes tab");
    c(&mut a, '5');
    assert_eq!(a.tab, Tab::Power);
    c(&mut a, '6');
    assert_eq!(a.tab, Tab::Displays);
    c(&mut a, '7');
    assert_eq!(a.tab, Tab::Appearance);
    k(&mut a, KeyCode::Esc);
    assert_eq!(a.tab, Tab::Home);
    k(&mut a, KeyCode::Esc);
    assert_eq!(a.tab, Tab::Home);
    assert!(!a.should_quit, "Esc does not quit");
}

#[test]
fn movement() {
    let mut a = ready_app();
    k(&mut a, KeyCode::Down);
    c(&mut a, 'h'); // Dvorak: down
    assert_eq!(a.focus, 2);
    k(&mut a, KeyCode::End);
    assert_eq!(a.focus, 6);
    k(&mut a, KeyCode::Down);
    assert_eq!(a.focus, 6, "bounded");
    k(&mut a, KeyCode::PageUp);
    assert_eq!(a.focus, 1);
    k(&mut a, KeyCode::Home);
    assert_eq!(a.focus, 0);
}

#[test]
fn slider_from_the_keyboard() {
    let mut a = ready_app();
    press(&mut a, KeyCode::Char('+'), KeyModifiers::SHIFT);
    k(&mut a, KeyCode::Right);
    c(&mut a, '-');
    assert_eq!(
        a.take_requests(),
        vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 67)),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 72)),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 67)),
        ]
    );
}

#[test]
fn space_only_toggles_switches() {
    let mut a = ready_app();
    c(&mut a, ' '); // on the Volume slider: nothing
    assert!(a.take_requests().is_empty());
    a.focus = 2;
    c(&mut a, ' ');
    assert_eq!(a.toggle_loadable(ToggleId::Wifi).ready(), Some(&false));
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false))]);
}

#[test]
fn confirmation_from_the_keyboard() {
    let mut a = ready_app();
    c(&mut a, '5');
    a.focus = 4; // Power off
    k(&mut a, KeyCode::Enter);
    assert_eq!(a.overlay, Overlay::Confirm(Confirm { action: PowerAction::Poweroff, yes_focused: false }));
    c(&mut a, 'q');
    assert!(!a.should_quit, "q cancels the confirmation, does not quit");
    assert_eq!(a.overlay, Overlay::None);
    k(&mut a, KeyCode::Enter);
    k(&mut a, KeyCode::Enter); // focus on Cancel
    assert_eq!(a.overlay, Overlay::None);
    a.take_requests();
    k(&mut a, KeyCode::Enter);
    k(&mut a, KeyCode::Right);
    k(&mut a, KeyCode::Enter);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Poweroff))]);
    k(&mut a, KeyCode::Enter);
    k(&mut a, KeyCode::Esc);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn arrow_keys_in_the_confirmation() {
    let mut a = ready_app();
    a.run_command(crate::app::action::Command::Power(PowerAction::Poweroff), Instant::now());
    let focus = |a: &App| match a.overlay {
        Overlay::Confirm(c) => c.yes_focused,
        _ => panic!("confirmation closed"),
    };
    k(&mut a, KeyCode::Left);
    assert!(!focus(&a), "← from Cancel stays on Cancel");
    k(&mut a, KeyCode::Right);
    assert!(focus(&a));
    k(&mut a, KeyCode::Right);
    assert!(focus(&a), "→ from Confirm stays on Confirm");
    k(&mut a, KeyCode::Left);
    assert!(!focus(&a));
    c(&mut a, 'n'); // Dvorak: right
    assert!(focus(&a));
    c(&mut a, 'd'); // Dvorak: left
    assert!(!focus(&a));
    k(&mut a, KeyCode::Tab);
    assert!(focus(&a), "Tab toggles");
    k(&mut a, KeyCode::Tab);
    assert!(!focus(&a));
    press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert!(focus(&a), "Shift+Tab toggles");
    assert!(a.take_requests().is_empty(), "nothing is launched without Enter");
}

#[test]
fn palette_from_the_keyboard() {
    let mut a = ready_app();
    press(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_eq!(a.overlay, Overlay::Palette(Palette::default()));
    typed(&mut a, "qx");
    assert!(!a.should_quit);
    k(&mut a, KeyCode::Backspace);
    k(&mut a, KeyCode::Backspace);
    typed(&mut a, "wifi");
    k(&mut a, KeyCode::Enter);
    assert_eq!(a.overlay, Overlay::None);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false))]);
    press(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
    k(&mut a, KeyCode::Down);
    k(&mut a, KeyCode::Down);
    k(&mut a, KeyCode::Up);
    assert_eq!(a.overlay, Overlay::Palette(Palette { query: String::new(), selected: 1 }));
    press(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_eq!(a.overlay, Overlay::None, "Ctrl+P closes it");
}

#[test]
fn help() {
    let mut a = ready_app();
    press(&mut a, KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert_eq!(a.overlay, Overlay::Help);
    k(&mut a, KeyCode::Esc);
    assert_eq!(a.overlay, Overlay::None);
    k(&mut a, KeyCode::F(1));
    k(&mut a, KeyCode::F(1));
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn search() {
    let mut a = ready_app();
    c(&mut a, '/');
    assert!(!a.search.editing, "no list on Home");
    c(&mut a, '2');
    c(&mut a, '/');
    assert!(a.search.editing);
    typed(&mut a, "casq");
    assert_eq!(a.items().len(), 2);
    k(&mut a, KeyCode::Enter);
    assert!(!a.search.editing);
    assert_eq!(a.search.text, "casq");
    k(&mut a, KeyCode::Down);
    assert_eq!(a.focus, 1);
    k(&mut a, KeyCode::Esc);
    assert_eq!(a.search, Search::default());
    assert_eq!(a.tab, Tab::Sound, "the first Esc clears the filter");
}

#[test]
fn enter_on_an_output() {
    let mut a = ready_app();
    c(&mut a, '2');
    a.take_requests();
    k(&mut a, KeyCode::Enter);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDefault(64))]);
    assert_eq!(a.focused(), Some(Item::Slider(SliderId::Node(64))));
}

// ── Displays tab ─────────────────────────────────────────────────

use super::super::{Keep, SaveDialog};

fn displays_tab() -> App {
    let mut a = ready_app();
    c(&mut a, '6');
    a.take_requests();
    a
}

/// Answers the worker: the sent trial is applied.
fn applied(a: &mut App) {
    let r = a.take_requests();
    let Some(Request::Do(op)) = r.into_iter().next() else { panic!("expected a trial") };
    a.on_update(super::super::worker::Update::Done(op, Ok(())), Instant::now());
    a.take_requests();
}

fn rollback_sent(a: &mut App) -> bool {
    a.take_requests().iter().any(|r| matches!(r, Request::Do(Op::ApplyDisplays { undo: None, .. })))
}

/// The worker answers the rollback, then a fresh read arrives.
fn rollback_applied_and_reread(a: &mut App) {
    let r = a.take_requests();
    let Some(Request::Do(op)) = r.into_iter().find(|q| matches!(q, Request::Do(Op::ApplyDisplays { undo: None, .. }))) else {
        panic!("expected a rollback")
    };
    a.on_update(super::super::worker::Update::Done(op, Ok(())), Instant::now());
    a.on_update(super::super::worker::Update::Displays(Ok(crate::displays::testing::machine())), Instant::now());
    a.take_requests();
}

#[test]
fn profiles_from_the_keyboard() {
    let mut a = displays_tab();
    assert_eq!(a.focused(), Some(Item::DisplayProfiles));
    k(&mut a, KeyCode::Right);
    k(&mut a, KeyCode::Enter); // Bureau
    assert!(matches!(a.overlay, Overlay::Keep(_)));
    assert!(matches!(a.take_requests().as_slice(), [Request::Do(Op::ApplyDisplays { undo: Some(_), .. })]));
}

#[test]
fn keep_these_settings_from_the_keyboard() {
    let mut a = displays_tab();
    k(&mut a, KeyCode::Down); // Portable
    c(&mut a, ' ');
    applied(&mut a);
    k(&mut a, KeyCode::Enter); // focus on Cancel
    assert_eq!(a.overlay, Overlay::None);
    rollback_applied_and_reread(&mut a); // "Enter by reflex: rollback"
    c(&mut a, ' ');
    applied(&mut a);
    k(&mut a, KeyCode::Right);
    assert_eq!(a.overlay, Overlay::Keep(Keep { keep_focused: true }));
    k(&mut a, KeyCode::Enter);
    assert_eq!(a.displays.trial, None);
    assert!(!rollback_sent(&mut a), "kept");
    a.on_update(super::super::worker::Update::Displays(Ok(crate::displays::testing::machine())), Instant::now());
    a.take_requests();
    c(&mut a, ' ');
    applied(&mut a);
    k(&mut a, KeyCode::Esc);
    assert!(rollback_sent(&mut a), "Esc cancels");
}

#[test]
fn q_during_the_trial_closes_and_leaves_the_rollback_for_exit() {
    let mut a = displays_tab();
    k(&mut a, KeyCode::Down);
    c(&mut a, ' ');
    applied(&mut a);
    c(&mut a, 'q');
    assert!(a.should_quit);
    assert!(a.exit_undo().is_some(), "the exit cleanup will undo the trial");
}

#[test]
fn panel_from_the_keyboard() {
    let mut a = displays_tab();
    k(&mut a, KeyCode::Down);
    k(&mut a, KeyCode::Enter); // panel of the laptop panel
    let Overlay::Panel(p) = &a.overlay else { panic!("expected a panel") };
    assert_eq!(p.output, "eDP-1");
    k(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Char('+'), KeyModifiers::SHIFT);
    let Overlay::Panel(p) = &a.overlay else { panic!() };
    assert_eq!((p.field, p.scale), (PanelField::Scale, crate::displays::Scale(1250)));
    k(&mut a, KeyCode::Enter); // to Apply
    k(&mut a, KeyCode::Enter); // applies
    assert!(matches!(a.overlay, Overlay::Keep(_)));
    assert!(matches!(a.take_requests().as_slice(), [Request::Do(Op::ApplyDisplays { undo: Some(_), .. })]));
}

#[test]
fn mode_search_from_the_keyboard() {
    let mut a = displays_tab();
    a.data.displays = super::super::Loadable::Ready(
        crate::backends::displays::parse_outputs(include_str!("../../../tests/fixtures/wm-set-outputs.json")).unwrap(),
    );
    k(&mut a, KeyCode::Down);
    k(&mut a, KeyCode::Down); // Alpha
    k(&mut a, KeyCode::Enter);
    c(&mut a, '/');
    typed(&mut a, "1280q"); // "q" is typed, closes nothing
    let Overlay::Panel(p) = &a.overlay else { panic!("panel closed") };
    assert_eq!(p.query.as_deref(), Some("1280q"));
    k(&mut a, KeyCode::Backspace);
    let Overlay::Panel(p) = &a.overlay else { panic!() };
    assert_eq!(p.mode.to_string(), "1280x1024@75");
    k(&mut a, KeyCode::Esc);
    let Overlay::Panel(p) = &a.overlay else { panic!("Esc closes the search first") };
    assert_eq!((p.query.clone(), p.mode.to_string()), (None, "1280x1024@75".to_string()));
    k(&mut a, KeyCode::Esc);
    assert_eq!(a.overlay, Overlay::None);
    assert!(a.take_requests().is_empty());
}

#[test]
fn saving_from_the_keyboard() {
    let mut a = displays_tab();
    k(&mut a, KeyCode::Right);
    k(&mut a, KeyCode::Right); // "+ Save current"
    k(&mut a, KeyCode::Enter);
    typed(&mut a, "Bureaux");
    k(&mut a, KeyCode::Backspace);
    assert_eq!(a.overlay, Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), ..SaveDialog::default() }));
    k(&mut a, KeyCode::Enter);
    assert_eq!(a.overlay, Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), replace: true, yes_focused: false }));
    k(&mut a, KeyCode::Right);
    k(&mut a, KeyCode::Esc);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn release_is_ignored() {
    let mut a = ready_app();
    let mut e = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    e.kind = KeyEventKind::Release;
    e.state = KeyEventState::NONE;
    a.on_key(e, Instant::now());
    assert!(!a.should_quit);
}
