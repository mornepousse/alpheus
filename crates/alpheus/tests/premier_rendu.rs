//! Critère de la spec : premier rendu < 100 ms, même si chaque outil met
//! 500 ms à répondre (le rendu n'attend aucun outil).

use alpheus::app::action::Tab;
use alpheus::app::run::build_app;
use alpheus::app::worker;
use alpheus::backends::Backends;
use alpheus::exec::fake::FakeExecutor;
use alpheus::ui;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn premier_rendu_sous_100_ms_meme_avec_des_outils_lents() {
    let start = Instant::now();
    let dir = std::env::temp_dir().join("alpheus-premier-rendu-sans-config");
    let mut app = build_app(&dir, Tab::Accueil, start);
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
    assert!(elapsed < Duration::from_millis(100), "premier rendu en {elapsed:?}");
    let buf = term.backend().buffer();
    let text: String = (0..30).flat_map(|y| (0..100).map(move |x| (x, y))).map(|p| buf[p].symbol().to_string()).collect();
    assert!(text.contains("Volume") && text.contains('…'), "l'écran montre l'attente, pas du vide");
}
