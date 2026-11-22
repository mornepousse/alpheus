//! Spec criterion: first render < 100 ms, even if every tool takes 500 ms
//! to answer (rendering never waits for any tool).

use alpheus::app::action::Tab;
use alpheus::app::run::build_app;
use alpheus::app::worker;
use alpheus::backends::Backends;
use alpheus::exec::fake::FakeExecutor;
use alpheus::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn first_render_under_100_ms_even_with_slow_tools() {
    let start = Instant::now();
    let dir = std::env::temp_dir().join("alpheus-first-render-no-config");
    let mut app = build_app(&dir, Tab::Home, start);
    let exec = Arc::new(FakeExecutor::with_delay(Duration::from_millis(500)));
    let (tx, _rx) = worker::spawn(Backends::system(exec, dir.clone()));
    for r in app.take_requests() {
        tx.send(r).unwrap();
    }
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| {
        ui::render(f, &app);
    })
    .unwrap();
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(100), "first render in {elapsed:?}");
    let buf = term.backend().buffer();
    let text: String = (0..30).flat_map(|y| (0..100).map(move |x| (x, y))).map(|p| buf[p].symbol().to_string()).collect();
    assert!(text.contains("Volume") && text.contains('…'), "the screen shows waiting, not emptiness");
}

#[test]
fn first_render_of_the_dashboard_under_100_ms_even_with_slow_tools() {
    let start = Instant::now();
    let dir = std::env::temp_dir().join("alpheus-first-render-dashboard-no-config");
    let mut app = build_app(&dir, Tab::Home, start);
    let exec = Arc::new(FakeExecutor::with_delay(Duration::from_millis(500)));
    let (tx, _rx) = worker::spawn(Backends::system(exec, dir.clone()));
    for r in app.take_requests() {
        tx.send(r).unwrap();
    }
    // 120×36: the rail and a 100×30 body, the dashboard's full layout.
    let mut term = Terminal::new(TestBackend::new(120, 36)).unwrap();
    term.draw(|f| {
        ui::render(f, &app);
    })
    .unwrap();
    let elapsed = start.elapsed();
    println!("first render of the dashboard in {elapsed:?}");
    assert!(elapsed < Duration::from_millis(100), "first render in {elapsed:?}");
    let buf = term.backend().buffer();
    let text: String = (0..36).flat_map(|y| (0..120).map(move |x| (x, y))).map(|p| buf[p].symbol().to_string()).collect();
    assert!(text.contains("Volume") && text.contains('…'), "the screen shows waiting, not emptiness");
    let days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    assert!(days.iter().any(|d| text.contains(d)), "the clock's date: {text}");
}

#[test]
fn first_render_of_diagnostics_under_100_ms_even_with_slow_tools() {
    let start = Instant::now();
    let dir = std::env::temp_dir().join("alpheus-first-render-diagnostics-no-config");
    let mut app = build_app(&dir, Tab::Diagnostics, start);
    // Every systemd tool takes half a second, captures included: the screen does not wait.
    let exec = Arc::new(FakeExecutor::with_delay(Duration::from_millis(500)));
    let (tx, _rx) = worker::spawn(Backends::system(exec, dir.clone()));
    let requests = app.take_requests();
    assert!(!requests.is_empty(), "the tab asks for its reads on opening");
    for r in requests {
        tx.send(r).unwrap();
    }
    let mut term = Terminal::new(TestBackend::new(120, 36)).unwrap();
    term.draw(|f| {
        ui::render(f, &app);
    })
    .unwrap();
    let elapsed = start.elapsed();
    println!("first render of the Diag tab in {elapsed:?}");
    assert!(elapsed < Duration::from_millis(100), "first render in {elapsed:?}");
    let buf = term.backend().buffer();
    let text: String = (0..36).flat_map(|y| (0..120).map(move |x| (x, y))).map(|p| buf[p].symbol().to_string()).collect();
    assert!(text.to_lowercase().contains("session health") && text.contains('…'), "the screen shows waiting, not emptiness: {text}");
}
